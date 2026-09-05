//! SQLite-backed durable state.
//!
//! The in-memory structures in [`crate::daemon::Daemon`] are the source of
//! truth for reads; every mutation is written through here so a restart can
//! rebuild them. Records are stored as JSON blobs beside the few columns
//! needed for lookups, so adding a field to a core type never needs a
//! migration — only a changed meaning does.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use agentdocker_core::{AgentId, AgentRecord, Change, Envelope, Event, Lease, LeaseId, ProjectId};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA_VERSION: i64 = 2;

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
CREATE TABLE IF NOT EXISTS changes (
    seq      INTEGER PRIMARY KEY AUTOINCREMENT,
    project  TEXT NOT NULL,
    path     TEXT NOT NULL,
    by_agent TEXT,
    at       TEXT NOT NULL,
    json     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS changes_project_seq ON changes (project, seq);
CREATE INDEX IF NOT EXISTS changes_project_path ON changes (project, path, seq);
";

pub struct Store {
    conn: Connection,
}

/// A ledger query; see [`Store::changes`].
#[derive(Debug, Clone)]
pub struct ChangesQuery {
    pub project: ProjectId,
    pub since_seq: Option<u64>,
    /// Relative to the checkout.
    pub path: Option<String>,
    pub agent: Option<AgentId>,
    pub limit: usize,
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
            Some(Ok(1)) => {
                // v2 adds stopping status and physical lease identities. The
                // daemon idempotently maps any remaining legacy file keys on load.
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

    // ----- changes (the ledger) ------------------------------------------

    /// Append a ledger entry and return its `seq`.
    pub fn append_change(&self, change: &Change) -> Result<u64> {
        self.conn.execute(
            "INSERT INTO changes (project, path, by_agent, at, json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                change.project.as_str(),
                change.path.to_string_lossy(),
                change.by.agent().map(AgentId::as_str),
                change.at.to_rfc3339(),
                serde_json::to_string(change)?,
            ],
        )?;
        let seq = u64::try_from(self.conn.last_insert_rowid()).unwrap_or(0);
        // The blob carries its own seq so a row reads back complete.
        let mut stored = change.clone();
        stored.seq = seq;
        self.conn.execute(
            "UPDATE changes SET json = ?1 WHERE seq = ?2",
            params![
                serde_json::to_string(&stored)?,
                i64::try_from(seq).unwrap_or(i64::MAX)
            ],
        )?;
        Ok(seq)
    }

    /// The newest `limit` entries matching the query, oldest first. A path
    /// matches itself and, as a directory, everything beneath it.
    pub fn changes(&self, query: &ChangesQuery) -> Result<Vec<Change>> {
        let mut sql = String::from("SELECT json FROM changes WHERE project = ?1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(query.project.as_str().to_owned())];
        if let Some(since) = query.since_seq {
            args.push(Box::new(i64::try_from(since).unwrap_or(i64::MAX)));
            sql.push_str(&format!(" AND seq > ?{}", args.len()));
        }
        if let Some(path) = &query.path {
            let path = path.trim_end_matches('/');
            args.push(Box::new(path.to_owned()));
            let exact = args.len();
            args.push(Box::new(format!("{path}/")));
            let lower = args.len();
            args.push(Box::new(format!("{path}0")));
            let upper = args.len();
            sql.push_str(&format!(
                " AND (path = ?{exact} OR (path >= ?{lower} AND path < ?{upper}))"
            ));
        }
        if let Some(agent) = &query.agent {
            args.push(Box::new(agent.as_str().to_owned()));
            sql.push_str(&format!(" AND by_agent = ?{}", args.len()));
        }
        args.push(Box::new(i64::try_from(query.limit).unwrap_or(i64::MAX)));
        sql.push_str(&format!(" ORDER BY seq DESC LIMIT ?{}", args.len()));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            |row| row.get::<_, String>(0),
        )?;
        let mut changes: Vec<Change> = rows
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_>>()?;
        changes.reverse();
        Ok(changes)
    }

    /// Keep only the newest `keep` entries per project. Returns how many went.
    pub fn prune_changes(&self, keep: usize) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT project FROM changes")?;
        let projects: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        let mut removed = 0;
        for project in projects {
            removed += self.conn.execute(
                "DELETE FROM changes WHERE project = ?1 AND seq NOT IN (
                     SELECT seq FROM changes WHERE project = ?1 ORDER BY seq DESC LIMIT ?2
                 )",
                params![project, i64::try_from(keep).unwrap_or(i64::MAX)],
            )?;
        }
        Ok(removed)
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

    #[test]
    fn ledger_queries_by_path_prefix_agent_and_seq_then_prunes() {
        use agentdocker_core::{Attribution, Change, ChangeKind, ProjectId};
        let store = Store::in_memory().unwrap();
        let project = ProjectId::from("p1");
        let entry = |path: &str, agent: Option<&str>| Change {
            seq: 0,
            project: project.clone(),
            worktree: None,
            path: path.into(),
            kind: ChangeKind::Modified,
            at: Utc::now(),
            by: match agent {
                Some(a) => Attribution::Agent {
                    agent: AgentId::from(a),
                    lease: agentdocker_core::LeaseId::from("l"),
                    note: None,
                },
                None => Attribution::External,
            },
            head: None,
        };
        let s1 = store
            .append_change(&entry("src/lib.rs", Some("a1")))
            .unwrap();
        let s2 = store.append_change(&entry("src/main.rs", None)).unwrap();
        let s3 = store
            .append_change(&entry("srcs/other.rs", Some("a1")))
            .unwrap();
        let s4 = store
            .append_change(&entry("README.md", Some("a2")))
            .unwrap();
        assert!(s1 < s2 && s2 < s3 && s3 < s4);
        let other = Change {
            project: ProjectId::from("p2"),
            ..entry("src/lib.rs", None)
        };
        store.append_change(&other).unwrap();

        let query = |path: Option<&str>, agent: Option<&str>, since: Option<u64>, limit: usize| {
            store
                .changes(&ChangesQuery {
                    project: project.clone(),
                    since_seq: since,
                    path: path.map(str::to_owned),
                    agent: agent.map(AgentId::from),
                    limit,
                })
                .unwrap()
                .into_iter()
                .map(|c| (c.seq, c.path.to_string_lossy().into_owned()))
                .collect::<Vec<_>>()
        };
        let paths = |rows: Vec<(u64, String)>| rows.into_iter().map(|r| r.1).collect::<Vec<_>>();
        assert_eq!(
            paths(query(None, None, None, 50)),
            ["src/lib.rs", "src/main.rs", "srcs/other.rs", "README.md"]
        );
        assert_eq!(
            paths(query(Some("src"), None, None, 50)),
            ["src/lib.rs", "src/main.rs"],
            "prefix, not srcs/"
        );
        assert_eq!(
            paths(query(Some("src/"), None, None, 50)),
            ["src/lib.rs", "src/main.rs"]
        );
        assert_eq!(
            paths(query(Some("src/lib.rs"), None, None, 50)),
            ["src/lib.rs"]
        );
        assert_eq!(
            paths(query(None, Some("a1"), None, 50)),
            ["src/lib.rs", "srcs/other.rs"]
        );
        assert_eq!(
            paths(query(None, None, Some(s2), 50)),
            ["srcs/other.rs", "README.md"]
        );
        assert_eq!(
            paths(query(None, None, None, 2)),
            ["srcs/other.rs", "README.md"],
            "newest two, oldest first"
        );
        let stored = store
            .changes(&ChangesQuery {
                project: project.clone(),
                since_seq: None,
                path: Some("README.md".into()),
                agent: None,
                limit: 1,
            })
            .unwrap();
        assert_eq!(stored[0].seq, s4, "the blob carries its seq");

        assert_eq!(store.prune_changes(2).unwrap(), 2);
        assert_eq!(
            paths(query(None, None, None, 50)),
            ["srcs/other.rs", "README.md"]
        );
        assert_eq!(
            store
                .changes(&ChangesQuery {
                    project: ProjectId::from("p2"),
                    since_seq: None,
                    path: None,
                    agent: None,
                    limit: 50
                })
                .unwrap()
                .len(),
            1,
            "other projects untouched"
        );
    }
}
