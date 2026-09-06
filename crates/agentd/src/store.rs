//! SQLite-backed durable state.
//!
//! The in-memory structures in [`crate::daemon::Daemon`] are the source of
//! truth for reads; every mutation is written through here so a restart can
//! rebuild them. Records are stored as JSON blobs beside the few columns
//! needed for lookups, so adding a field to a core type never needs a
//! migration — only a changed meaning does.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use agentdocker_core::{
    AgentId, AgentRecord, Change, Envelope, Event, JournalEntry, JournalKind, Lease, LeaseId,
    ProjectId,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA_VERSION: i64 = 6;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS documents (
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    json TEXT NOT NULL,
    PRIMARY KEY (kind, id)
);
CREATE INDEX IF NOT EXISTS documents_agent ON documents (kind, json_extract(json, '$.agent'));
CREATE INDEX IF NOT EXISTS documents_author ON documents (kind, json_extract(json, '$.from'));
CREATE INDEX IF NOT EXISTS documents_version ON documents (kind, json_extract(json, '$.checkout'), json_extract(json, '$.before'));
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
CREATE TABLE IF NOT EXISTS journal (
    id      INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    seq     INTEGER NOT NULL,
    at      TEXT NOT NULL,
    agent   TEXT,
    branch  TEXT,
    kind    TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    json    TEXT NOT NULL,
    UNIQUE (project, seq)
);
CREATE TABLE IF NOT EXISTS journal_heads (
    project TEXT PRIMARY KEY,
    seq INTEGER NOT NULL
);
INSERT INTO journal_heads (project, seq) SELECT project, MAX(seq) FROM journal GROUP BY project
ON CONFLICT(project) DO UPDATE SET seq = MAX(journal_heads.seq, excluded.seq);
CREATE INDEX IF NOT EXISTS journal_branch ON journal (project, branch, seq);
CREATE INDEX IF NOT EXISTS journal_agent ON journal (project, agent, seq);
CREATE TABLE IF NOT EXISTS journal_paths (
    project TEXT NOT NULL,
    path    TEXT NOT NULL,
    seq     INTEGER NOT NULL,
    PRIMARY KEY (project, path, seq)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS journal_cursors (
    agent      TEXT NOT NULL,
    project    TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (agent, project)
) WITHOUT ROWID;
";

/// Full-text search over journal summaries. Contentless: the text lives in
/// the journal row, the index only maps terms to `journal.id`.
const JOURNAL_FTS: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS journal_fts USING fts5(summary, content='', contentless_delete=1)";

pub struct Store {
    conn: Connection,
    /// Whether the SQLite build gave us FTS5; `--grep` falls back to LIKE.
    fts: bool,
}

/// A journal query; see [`Store::journal`].
#[derive(Debug, Clone)]
pub struct JournalQuery {
    pub project: ProjectId,
    pub since_seq: Option<u64>,
    pub until_seq: Option<u64>,
    pub agent: Option<AgentId>,
    pub branch: Option<String>,
    pub kind: Option<JournalKind>,
    /// Checkout-relative; a directory matches everything beneath it.
    pub path: Option<String>,
    pub grep: Option<String>,
    pub limit: usize,
}

impl JournalQuery {
    /// Everything in a project, newest `limit`.
    pub fn new(project: ProjectId, limit: usize) -> Self {
        Self {
            project,
            since_seq: None,
            until_seq: None,
            agent: None,
            branch: None,
            kind: None,
            path: None,
            grep: None,
            limit,
        }
    }
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
    /// Only changes seen at or after this time.
    pub after: Option<chrono::DateTime<chrono::Utc>>,
    /// Only changes below this sequence number: the page before one
    /// already read, for a reader that walks the ledger newest first.
    pub before_seq: Option<u64>,
}

impl Store {
    /// Container identity/status, exit lease cleanup and replay history commit together.
    pub fn container_transition(
        &self,
        record: &AgentRecord,
        leases: &[LeaseId],
        journal: &[JournalEntry],
        events: &[Event],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.upsert_agent(record)?;
        for id in leases {
            self.conn
                .execute("DELETE FROM leases WHERE id = ?1", params![id.as_str()])?;
        }
        for entry in journal {
            self.insert_journal(entry)?;
        }
        for event in events {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Commit acceptance and inherited observations in the same transaction.
    pub fn put_document_with_event<T: serde::Serialize + ?Sized>(
        &self,
        kind: &str,
        id: &str,
        value: &T,
        event: &Event,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.put_document(kind, id, value)?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// Commit acceptance, the inherited read set, any leases that moved
    /// to the recipient, and every event announcing it, in one
    /// transaction.
    pub fn accept_handoff(
        &self,
        checkpoint: &agentdocker_core::Checkpoint,
        agent: &AgentId,
        reads: &[agentdocker_core::ReadMark],
        transferred: &[Lease],
        cursor: Option<(&str, &ProjectId, u64, DateTime<Utc>)>,
        events: &[Event],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.put_document("checkpoint", &checkpoint.id, checkpoint)?;
        self.put_document("reads", agent.as_str(), &reads)?;
        for lease in transferred {
            self.upsert_lease(lease)?;
        }
        if let Some((reader, project, seq, now)) = cursor {
            self.set_journal_cursor(reader, project, seq, now)?;
        }
        for event in events {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Store a bundle brought from elsewhere with the checkpoint that
    /// `resume` will accept it under, and the event, together.
    pub fn import_handoff(
        &self,
        checkpoint: &agentdocker_core::Checkpoint,
        bundle: &agentdocker_core::HandoffBundle,
        event: &Event,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.put_document("checkpoint", &checkpoint.id, checkpoint)?;
        self.put_document("handoff", &bundle.id, bundle)?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// Bundles an agent sent or is addressed to; all of them without one.
    /// Oldest first.
    pub fn handoffs(
        &self,
        agent: Option<&AgentId>,
    ) -> Result<Vec<agentdocker_core::HandoffBundle>> {
        let mut stmt = self.conn.prepare(
            "SELECT json FROM documents WHERE kind='handoff'
             AND (?1 IS NULL OR json_extract(json, '$.from') = ?1 OR json_extract(json, '$.to') = ?1)
             ORDER BY json_extract(json, '$.created_at'), id",
        )?;
        let rows = stmt.query_map(params![agent.map(AgentId::as_str)], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    /// Return recovery documents in stable id order.
    pub fn documents<T: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        agent: Option<&AgentId>,
    ) -> Result<Vec<T>> {
        let mut stmt = self
            .conn
            .prepare("SELECT json FROM documents WHERE kind=?1 AND (?2 IS NULL OR json_extract(json, '$.agent') = ?2 OR json_extract(json, '$.from') = ?2) ORDER BY id")?;
        let rows = stmt.query_map(params![kind, agent.map(AgentId::as_str)], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    /// Select only passing evidence for the checkpoint's exact content scope.
    pub fn matching_validations(
        &self,
        checkout: &Path,
        version: &str,
    ) -> Result<Vec<agentdocker_core::Validation>> {
        let mut stmt = self.conn.prepare(
            "SELECT json FROM documents WHERE kind='validation'
            AND json_extract(json, '$.checkout')=?1 AND json_extract(json, '$.before')=?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![checkout.to_string_lossy(), version], |row| {
            row.get::<_, String>(0)
        })?;
        let validations: Vec<agentdocker_core::Validation> = rows
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_>>()?;
        Ok(validations
            .into_iter()
            .filter(agentdocker_core::Validation::passed)
            .collect())
    }

    /// Atomically persist a typed recovery document before publishing its event.
    pub fn put_document<T: serde::Serialize + ?Sized>(
        &self,
        kind: &str,
        id: &str,
        value: &T,
    ) -> Result<()> {
        self.conn.execute("INSERT INTO documents (kind,id,json) VALUES (?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET json=excluded.json",
            params![kind,id,serde_json::to_string(value)?])?;
        Ok(())
    }
    /// Load a durable observation or recovery document.
    pub fn document<T: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Option<T>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM documents WHERE kind=?1 AND id=?2",
                params![kind, id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }
    #[cfg(test)]
    pub(crate) fn reject_event_for_test(&self, kind: &str) {
        // Only static test event names enter this trigger.
        assert!(kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        self.conn
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER reject_event BEFORE INSERT ON events
            WHEN json_extract(NEW.json, '$.kind.event') = '{kind}'
            BEGIN SELECT RAISE(FAIL, 'injected event failure'); END;"
            ))
            .unwrap();
    }

    #[cfg(test)]
    pub(crate) fn reject_writes_for_test(&self) {
        self.conn.execute_batch("PRAGMA query_only=ON").unwrap();
    }

    #[cfg(test)]
    pub(crate) fn reject_validation_finish_for_test(&self) {
        self.conn.execute_batch("CREATE TEMP TRIGGER reject_validation_finish
            BEFORE INSERT ON events WHEN json_extract(NEW.json, '$.kind.event') = 'validation_finished'
            BEGIN SELECT RAISE(FAIL, 'injected validation event failure'); END;").unwrap();
    }

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

    /// Behave as if the SQLite build lacked FTS5, to exercise the fallback.
    #[cfg(test)]
    pub fn without_fts(mut self) -> Self {
        self.fts = false;
        self
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(SCHEMA)?;
        // A journal written before `summary` had its own column gets one,
        // filled from the blob, so the LIKE fallback searches the same text
        // as FTS. Idempotent: the column is checked for, not the version.
        let has_summary = conn
            .prepare("PRAGMA table_info(journal)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|column| column == "summary");
        if !has_summary {
            let tx = conn.unchecked_transaction()?;
            conn.execute_batch(
                "ALTER TABLE journal ADD COLUMN summary TEXT NOT NULL DEFAULT '';
                 UPDATE journal SET summary = COALESCE(json_extract(json, '$.summary'), '')",
            )?;
            tx.commit()?;
        }

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
            Some(Ok(1..=5)) => {
                // v2 adds stopping status and physical lease identities; v3
                // records dedicated process groups. Legacy groups default to
                // None. v4 distinguishes container lifetime from host PIDs.
                // The daemon maps legacy file keys idempotently on load.
                conn.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                    params![SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(other) => anyhow::bail!(
                "state database has schema version {other:?}; this build expects {SCHEMA_VERSION}"
            ),
        }
        let had_fts: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='journal_fts')",
            [],
            |row| row.get(0),
        )?;
        let fts = match conn.execute_batch(JOURNAL_FTS) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(%err, "FTS5 unavailable; journal --grep falls back to LIKE");
                false
            }
        };
        if fts {
            let complete: Option<String> = conn
                .query_row(
                    "SELECT value FROM meta WHERE key='journal_fts_complete'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            if !had_fts || complete.as_deref() != Some("1") {
                let tx = conn.unchecked_transaction()?;
                conn.execute(
                    "INSERT INTO journal_fts(journal_fts) VALUES('delete-all')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO journal_fts(rowid, summary) SELECT id, summary FROM journal",
                    [],
                )?;
                conn.execute(
                    "INSERT OR REPLACE INTO meta(key, value) VALUES('journal_fts_complete', '1')",
                    [],
                )?;
                tx.commit()?;
            }
        }
        Ok(Self { conn, fts })
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
        self.conn.execute(
            "DELETE FROM journal_cursors WHERE agent = ?1",
            params![id.as_str()],
        )?;
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

    /// A removed lease and its replay evidence must survive or roll back together.
    pub fn delete_lease_with_event(&self, id: &LeaseId, event: &Event) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.delete_lease(id)?;
        self.append_event(event)?;
        tx.commit()?;
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
        let tx = self.conn.unchecked_transaction()?;
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
        tx.commit()?;
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
        event: &Event,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for message in messages {
            tx.execute(
                "DELETE FROM inbox WHERE agent = ?1 AND message_id = ?2",
                params![agent.as_str(), message.as_str()],
            )?;
        }
        self.append_event(event)?;
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

    // ----- changes (the ledger) ------------------------------------------

    /// Append a ledger entry and return its `seq`.
    pub fn append_change(&self, change: &Change) -> Result<u64> {
        let tx = self.conn.unchecked_transaction()?;
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
        tx.commit()?;
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
        if let Some(path) = query
            .path
            .as_deref()
            .map(|p| p.trim_end_matches('/').trim_start_matches("./"))
            .filter(|p| !p.is_empty() && *p != ".")
        {
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
        if let Some(after) = &query.after {
            args.push(Box::new(after.to_rfc3339()));
            sql.push_str(&format!(" AND julianday(at) >= julianday(?{})", args.len()));
        }
        if let Some(before) = query.before_seq {
            args.push(Box::new(i64::try_from(before).unwrap_or(i64::MAX)));
            sql.push_str(&format!(" AND seq < ?{}", args.len()));
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

    // ----- journal --------------------------------------------------------

    /// The highest journal `seq` stored for a project, or 0.
    pub fn max_journal_seq(&self, project: &ProjectId) -> Result<u64> {
        let max: i64 = self.conn.query_row(
            "SELECT COALESCE((SELECT seq FROM journal_heads WHERE project = ?1), 0)",
            params![project.as_str()],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(max).unwrap_or(0))
    }

    /// Append an entry (its `seq` already assigned) on its own.
    #[cfg(test)]
    pub fn append_journal(&self, entry: &JournalEntry) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.insert_journal(entry)?;
        tx.commit()?;
        Ok(())
    }

    /// Delete released leases and append the entry that describes the
    /// release in one transaction, so a crash can leave neither a released
    /// lease without its entry nor an entry for a lease still held.
    pub fn release_leases(
        &self,
        leases: &[LeaseId],
        entry: Option<&JournalEntry>,
        events: &[Event],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for id in leases {
            self.conn
                .execute("DELETE FROM leases WHERE id = ?1", params![id.as_str()])?;
        }
        if let Some(entry) = entry {
            self.insert_journal(entry)?;
        }
        for event in events {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Journal and ordered replay event must survive or roll back together.
    pub fn append_journal_with_event(&self, entry: &JournalEntry, event: &Event) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.insert_journal(entry)?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// Acknowledgement must not advance past a failed replay event.
    pub fn set_journal_cursor_with_event(
        &self,
        key: &str,
        project: &ProjectId,
        seq: u64,
        event: &Event,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.set_journal_cursor(key, project, seq, event.at)?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// Last assigned ledger sequence, retained even when rows are pruned.
    pub fn change_watermark(&self) -> Result<u64> {
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='changes'), 0)",
            [],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(seq)?)
    }

    fn insert_journal(&self, entry: &JournalEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO journal_heads (project, seq) VALUES (?1, ?2)
            ON CONFLICT(project) DO UPDATE SET seq = MAX(journal_heads.seq, excluded.seq)",
            params![entry.project.as_str(), i64::try_from(entry.seq)?],
        )?;
        self.conn.execute(
            "INSERT INTO journal (project, seq, at, agent, branch, kind, summary, json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.project.as_str(),
                i64::try_from(entry.seq).unwrap_or(i64::MAX),
                entry.at.to_rfc3339(),
                entry.agent.as_ref().map(AgentId::as_str),
                entry.branch,
                entry.kind.to_string(),
                entry.summary,
                serde_json::to_string(entry)?,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        for path in &entry.paths {
            self.conn.execute(
                "INSERT OR IGNORE INTO journal_paths (project, path, seq) VALUES (?1, ?2, ?3)",
                params![
                    entry.project.as_str(),
                    path.to_string_lossy(),
                    i64::try_from(entry.seq).unwrap_or(i64::MAX)
                ],
            )?;
        }
        if self.fts {
            self.conn.execute(
                "INSERT INTO journal_fts (rowid, summary) VALUES (?1, ?2)",
                params![id, entry.summary],
            )?;
        } else {
            self.conn
                .execute("DELETE FROM meta WHERE key='journal_fts_complete'", [])?;
        }
        Ok(())
    }

    /// The newest `limit` entries matching the query, oldest first.
    pub fn journal(&self, query: &JournalQuery) -> Result<Vec<JournalEntry>> {
        let mut sql = String::from("SELECT json FROM journal WHERE project = ?1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(query.project.as_str().to_owned())];
        if let Some(since) = query.since_seq {
            args.push(Box::new(i64::try_from(since).unwrap_or(i64::MAX)));
            sql.push_str(&format!(" AND seq > ?{}", args.len()));
        }
        if let Some(until) = query.until_seq {
            args.push(Box::new(i64::try_from(until).unwrap_or(i64::MAX)));
            sql.push_str(&format!(" AND seq <= ?{}", args.len()));
        }
        if let Some(agent) = &query.agent {
            args.push(Box::new(agent.as_str().to_owned()));
            sql.push_str(&format!(" AND agent = ?{}", args.len()));
        }
        if let Some(branch) = &query.branch {
            args.push(Box::new(branch.clone()));
            sql.push_str(&format!(" AND branch = ?{}", args.len()));
        }
        if let Some(kind) = &query.kind {
            args.push(Box::new(kind.to_string()));
            sql.push_str(&format!(" AND kind = ?{}", args.len()));
        }
        if let Some(path) = query
            .path
            .as_deref()
            .map(|p| p.trim_end_matches('/').trim_start_matches("./"))
            .filter(|p| !p.is_empty() && *p != ".")
        {
            args.push(Box::new(path.to_owned()));
            let exact = args.len();
            args.push(Box::new(format!("{path}/")));
            let lower = args.len();
            args.push(Box::new(format!("{path}0")));
            let upper = args.len();
            sql.push_str(&format!(
                " AND seq IN (SELECT seq FROM journal_paths WHERE project = ?1 AND (path = ?{exact} OR (path >= ?{lower} AND path < ?{upper})))"
            ));
        }
        if let Some(grep) = query
            .grep
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // FTS has no tokens for punctuation-only text. Match that literal
            // text with LIKE whether or not FTS is available.
            if self.fts && grep.chars().any(char::is_alphanumeric) {
                // Quote the whole phrase so user text is never FTS syntax.
                args.push(Box::new(format!("\"{}\"", grep.replace('"', "\"\""))));
                sql.push_str(&format!(
                    " AND id IN (SELECT rowid FROM journal_fts WHERE journal_fts MATCH ?{})",
                    args.len()
                ));
            } else {
                // Same text as the FTS branch, and user text is never a
                // pattern: `%` and `_` are matched literally.
                let escaped = grep
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                args.push(Box::new(format!("%{escaped}%")));
                sql.push_str(&format!(" AND summary LIKE ?{} ESCAPE '\\'", args.len()));
            }
        }
        args.push(Box::new(
            i64::try_from(query.limit.max(1)).unwrap_or(i64::MAX),
        ));
        sql.push_str(&format!(" ORDER BY seq DESC LIMIT ?{}", args.len()));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            |row| row.get::<_, String>(0),
        )?;
        let mut entries: Vec<JournalEntry> = rows
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_>>()?;
        entries.reverse();
        Ok(entries)
    }

    /// Drop a project's entries below `before_seq`, with their paths and
    /// search rows. Returns how many entries went.
    pub fn prune_journal(&self, project: &ProjectId, before_seq: u64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let before = i64::try_from(before_seq).unwrap_or(i64::MAX);
        if self.fts {
            self.conn.execute(
                "DELETE FROM journal_fts WHERE rowid IN (SELECT id FROM journal WHERE project = ?1 AND seq < ?2)",
                params![project.as_str(), before],
            )?;
        } else {
            self.conn
                .execute("DELETE FROM meta WHERE key='journal_fts_complete'", [])?;
        }
        self.conn.execute(
            "DELETE FROM journal_paths WHERE project = ?1 AND seq < ?2",
            params![project.as_str(), before],
        )?;
        let removed = self.conn.execute(
            "DELETE FROM journal WHERE project = ?1 AND seq < ?2",
            params![project.as_str(), before],
        )?;
        tx.commit()?;
        Ok(removed)
    }

    // ----- journal cursors -----------------------------------------------

    /// The last entry a reader was shown in a project; `None` for a reader
    /// that has never been shown anything there.
    pub fn journal_cursor(&self, reader: &str, project: &ProjectId) -> Result<Option<u64>> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT seq FROM journal_cursors WHERE agent = ?1 AND project = ?2",
                params![reader, project.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(seq.map(|seq| u64::try_from(seq).unwrap_or(0)))
    }

    /// Record the last entry a reader was shown.
    pub fn set_journal_cursor(
        &self,
        reader: &str,
        project: &ProjectId,
        seq: u64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO journal_cursors (agent, project, seq, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent, project) DO UPDATE SET
                 seq = excluded.seq,
                 updated_at = excluded.updated_at",
            params![
                reader,
                project.as_str(),
                i64::try_from(seq).unwrap_or(i64::MAX),
                now.to_rfc3339()
            ],
        )?;
        Ok(())
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
    #[ignore = "manual filesystem durability benchmark"]
    fn durability_write_benchmark() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("state.db")).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..200 {
            store
                .enqueue(&AgentId::from("reader"), &envelope("message"), 1000)
                .unwrap();
        }
        let inbox = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..200 {
            store
                .append_change(&agentdocker_core::Change {
                    seq: 0,
                    project: ProjectId::from("project"),
                    checkout: Some(tmp.path().into()),
                    worktree: None,
                    path: "file".into(),
                    kind: agentdocker_core::ChangeKind::Modified,
                    at: Utc::now(),
                    by: agentdocker_core::Attribution::External,
                    head: None,
                })
                .unwrap();
        }
        eprintln!(
            "FULL durability, 200 operations each: enqueue={inbox:?}, append_change={:?}",
            start.elapsed()
        );
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
        let mut lease = Lease {
            id: LeaseId::generate(),
            resource: ResourceKey::new("task:1"),
            holder: AgentId::from("a"),
            mode: LeaseMode::Shared,
            acquired_at: now,
            change_seq: None,
            expires_at: now + Duration::seconds(30),
            note: Some("n".into()),
        };
        store.upsert_lease(&lease).unwrap();
        assert_eq!(store.load_leases().unwrap(), vec![lease.clone()]);
        lease.change_seq = Some(42);
        store.upsert_lease(&lease).unwrap();
        assert_eq!(store.load_leases().unwrap(), [lease.clone()]);
        store.delete_lease(&lease.id).unwrap();
        assert!(store.load_leases().unwrap().is_empty());
    }

    #[test]
    fn failed_removal_event_rolls_back_lease_deletion() {
        let store = Store::in_memory().unwrap();
        let now = Utc::now();
        let lease = Lease {
            id: LeaseId::generate(),
            resource: ResourceKey::new("task:atomic"),
            holder: AgentId::from("a"),
            mode: LeaseMode::Exclusive,
            acquired_at: now,
            change_seq: None,
            expires_at: now + Duration::seconds(30),
            note: None,
        };
        store.upsert_lease(&lease).unwrap();
        let mut event = Event::new(
            EventKind::LeaseReleased {
                lease: lease.clone(),
            },
            now,
        );
        event.seq = 1;
        store.append_event(&event).unwrap();
        assert!(store.delete_lease_with_event(&lease.id, &event).is_err());
        assert_eq!(store.load_leases().unwrap(), std::slice::from_ref(&lease));
        assert_eq!(store.recent_events(100).unwrap().len(), 1);
        event.seq = 2;
        store.delete_lease_with_event(&lease.id, &event).unwrap();
        assert!(store.load_leases().unwrap().is_empty());
        assert_eq!(store.recent_events(100).unwrap().last(), Some(&event));
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
    fn legacy_schemas_upgrade_to_container_lifetime_guard() {
        for version in 1..=5 {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version', ?1)",
                [version.to_string()],
            )
            .unwrap();
            let store = Store::init(conn).unwrap();
            let version: String = store
                .conn
                .query_row(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(version, "6");
        }
    }

    #[test]
    fn ledger_queries_by_path_prefix_agent_and_seq_then_prunes() {
        use agentdocker_core::{Attribution, Change, ChangeKind, ProjectId};
        let store = Store::in_memory().unwrap();
        let project = ProjectId::from("p1");
        let entry = |path: &str, agent: Option<&str>| Change {
            seq: 0,
            project: project.clone(),
            checkout: None,
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
                    after: None,
                    before_seq: None,
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
        for root in ["", ".", "./"] {
            assert_eq!(
                query(Some(root), None, None, 50),
                query(None, None, None, 50),
                "root filter {root:?} includes every project path"
            );
            assert_eq!(
                query(Some(root), Some("a1"), Some(s1), 1),
                query(None, Some("a1"), Some(s1), 1),
                "root normalization preserves the other filters"
            );
        }
        assert_eq!(
            paths(query(Some("src"), None, None, 50)),
            ["src/lib.rs", "src/main.rs"],
            "prefix, not srcs/"
        );
        assert_eq!(
            paths(query(Some("src/"), None, None, 50)),
            ["src/lib.rs", "src/main.rs"]
        );
        for relative in ["./src", "./src/", "././src"] {
            assert_eq!(
                query(Some(relative), None, None, 50),
                query(Some("src"), None, None, 50)
            );
        }
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
                after: None,
                before_seq: None,
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
                    limit: 50,
                    after: None,
                    before_seq: None,
                })
                .unwrap()
                .len(),
            1,
            "other projects untouched"
        );
    }

    #[test]
    fn journal_appends_queries_and_prunes_with_leases_in_one_transaction() {
        use agentdocker_core::{JournalEntry, JournalKind, ProjectId, SummarySource};
        let store = Store::in_memory().unwrap();
        let project = ProjectId::from("p1");
        let entry = |seq: u64,
                     kind: JournalKind,
                     summary: &str,
                     paths: &[&str],
                     agent: &str,
                     branch: &str| JournalEntry {
            project: project.clone(),
            seq,
            at: Utc::now(),
            agent: Some(AgentId::from(agent)),
            agent_name: agent.to_owned(),
            branch: Some(branch.to_owned()),
            checkout: None,
            worktree: None,
            kind,
            summary: summary.to_owned(),
            summary_source: SummarySource::Explicit,
            resources: Vec::new(),
            paths: paths.iter().map(|p| p.into()).collect(),
            paths_total: paths.len(),
            head_before: None,
            head_after: None,
            changes: None,
        };
        assert_eq!(store.max_journal_seq(&project).unwrap(), 0);

        // A release and its entry land together.
        let lease = Lease {
            id: LeaseId::from("l1"),
            resource: ResourceKey::new("path:/repo/src/a.rs"),
            holder: AgentId::from("a1"),
            mode: LeaseMode::Exclusive,
            acquired_at: Utc::now(),
            change_seq: None,
            expires_at: Utc::now() + Duration::seconds(60),
            note: None,
        };
        store.upsert_lease(&lease).unwrap();
        store
            .release_leases(
                std::slice::from_ref(&lease.id),
                Some(&entry(
                    1,
                    JournalKind::Release,
                    "rewrote the parser",
                    &["src/a.rs", "src/b.rs"],
                    "a1",
                    "main",
                )),
                &[],
            )
            .unwrap();
        assert!(store.load_leases().unwrap().is_empty());
        store
            .append_journal(&entry(
                2,
                JournalKind::Note,
                "lexer next",
                &[],
                "a1",
                "main",
            ))
            .unwrap();
        store
            .append_journal(&entry(
                3,
                JournalKind::Commit,
                "committed abc: Add lexer",
                &[],
                "a2",
                "feature",
            ))
            .unwrap();
        store
            .append_journal(&entry(
                4,
                JournalKind::Release,
                "touched docs",
                &["docs/x.md"],
                "a2",
                "feature",
            ))
            .unwrap();
        assert_eq!(store.max_journal_seq(&project).unwrap(), 4);

        let q = |f: &dyn Fn(&mut JournalQuery)| {
            let mut query = JournalQuery::new(project.clone(), 50);
            f(&mut query);
            store
                .journal(&query)
                .unwrap()
                .into_iter()
                .map(|e| e.seq)
                .collect::<Vec<_>>()
        };
        assert_eq!(q(&|_| {}), [1, 2, 3, 4]);
        assert_eq!(q(&|x| x.limit = 2), [3, 4], "newest two, oldest first");
        assert_eq!(q(&|x| x.since_seq = Some(2)), [3, 4]);
        assert_eq!(q(&|x| x.until_seq = Some(2)), [1, 2]);
        assert_eq!(q(&|x| x.agent = Some(AgentId::from("a2"))), [3, 4]);
        assert_eq!(q(&|x| x.branch = Some("main".into())), [1, 2]);
        assert_eq!(q(&|x| x.kind = Some(JournalKind::Release)), [1, 4]);
        assert_eq!(
            q(&|x| x.path = Some("src".into())),
            [1],
            "directory prefix via journal_paths"
        );
        assert_eq!(q(&|x| x.path = Some("src/b.rs".into())), [1]);
        assert_eq!(q(&|x| x.path = Some("srcs".into())), Vec::<u64>::new());
        assert_eq!(q(&|x| x.grep = Some("parser".into())), [1]);
        assert_eq!(
            q(&|x| x.grep = Some("lexer".into())),
            [2, 3],
            "fts or like, both match"
        );

        assert_eq!(store.prune_journal(&project, 3).unwrap(), 2);
        assert_eq!(q(&|_| {}), [3, 4]);
        assert_eq!(
            q(&|x| x.path = Some("src".into())),
            Vec::<u64>::new(),
            "paths pruned too"
        );
        assert_eq!(
            q(&|x| x.grep = Some("parser".into())),
            Vec::<u64>::new(),
            "search rows pruned too"
        );
    }

    #[test]
    fn journal_cursors_round_trip_and_go_with_their_agent() {
        use agentdocker_core::ProjectId;
        let store = Store::in_memory().unwrap();
        let project = ProjectId::from("p1");
        assert_eq!(store.journal_cursor("a1", &project).unwrap(), None);
        store
            .set_journal_cursor("a1", &project, 7, Utc::now())
            .unwrap();
        store
            .set_journal_cursor("a1", &project, 9, Utc::now())
            .unwrap();
        store
            .set_journal_cursor("user", &project, 3, Utc::now())
            .unwrap();
        assert_eq!(store.journal_cursor("a1", &project).unwrap(), Some(9));
        assert_eq!(store.journal_cursor("user", &project).unwrap(), Some(3));
        assert_eq!(
            store.journal_cursor("a1", &ProjectId::from("p2")).unwrap(),
            None,
            "one cursor per project"
        );
        store.delete_agent(&AgentId::from("a1")).unwrap();
        assert_eq!(store.journal_cursor("a1", &project).unwrap(), None);
        assert_eq!(store.journal_cursor("user", &project).unwrap(), Some(3));
    }

    #[test]
    fn like_fallback_searches_only_summaries_and_takes_text_literally() {
        use agentdocker_core::{JournalEntry, JournalKind, ProjectId, SummarySource};
        let project = ProjectId::from("p1");
        let entry = |seq: u64, summary: &str| JournalEntry {
            project: project.clone(),
            seq,
            at: Utc::now(),
            agent: Some(AgentId::from("agent-one")),
            agent_name: "codex-1".to_owned(),
            branch: Some("feat/lexer".to_owned()),
            checkout: None,
            worktree: None,
            kind: JournalKind::Note,
            summary: summary.to_owned(),
            summary_source: SummarySource::Explicit,
            resources: Vec::new(),
            paths: vec!["src/lexer.rs".into()],
            paths_total: 1,
            head_before: None,
            head_after: None,
            changes: None,
        };

        // A database from before the column existed: the blob is the only
        // copy of the summary until `init` adds and fills the column.
        let legacy = Connection::open_in_memory().unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE journal (
                    id INTEGER PRIMARY KEY, project TEXT NOT NULL, seq INTEGER NOT NULL,
                    at TEXT NOT NULL, agent TEXT, branch TEXT, kind TEXT NOT NULL,
                    json TEXT NOT NULL, UNIQUE (project, seq))",
            )
            .unwrap();
        let old = entry(1, "rewrote the parser");
        legacy
            .execute(
                "INSERT INTO journal (project, seq, at, agent, branch, kind, json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    "p1",
                    1,
                    old.at.to_rfc3339(),
                    "agent-one",
                    "feat/lexer",
                    "note",
                    serde_json::to_string(&old).unwrap()
                ],
            )
            .unwrap();
        let store = Store::init(legacy).unwrap().without_fts();
        store.append_journal(&entry(2, "100% of a_b done")).unwrap();

        let q = |grep: &str| {
            let mut query = JournalQuery::new(project.clone(), 50);
            query.grep = Some(grep.to_owned());
            store
                .journal(&query)
                .unwrap()
                .into_iter()
                .map(|e| e.seq)
                .collect::<Vec<_>>()
        };
        assert_eq!(q("parser"), [1], "backfilled from the blob");
        assert_eq!(q("PARSER"), [1], "LIKE is case-insensitive like FTS");
        assert_eq!(q("a_b"), [2]);
        assert_eq!(q("100%"), [2]);
        assert_eq!(q("aXb"), Vec::<u64>::new(), "`_` is not a wildcard");
        assert_eq!(q("100"), [2]);
        for not_summary in ["agent-one", "codex-1", "feat/lexer", "lexer.rs", "note"] {
            assert_eq!(
                q(not_summary),
                Vec::<u64>::new(),
                "{not_summary} is not summary text"
            );
        }
    }
    #[test]
    fn interrupted_summary_migration_rolls_back_and_can_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let legacy = Connection::open(&path).unwrap();
        legacy.execute_batch("CREATE TABLE journal (
            id INTEGER PRIMARY KEY, project TEXT NOT NULL, seq INTEGER NOT NULL,
            at TEXT NOT NULL, agent TEXT, branch TEXT, kind TEXT NOT NULL,
            json TEXT NOT NULL, UNIQUE(project, seq));
            INSERT INTO journal (project, seq, at, kind, json) VALUES ('p', 7, '', 'note', 'malformed');").unwrap();
        drop(legacy);
        assert!(Store::open(&path).is_err());
        let conn = Connection::open(&path).unwrap();
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(journal)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            !columns.iter().any(|column| column == "summary"),
            "ALTER TABLE must roll back with failed backfill"
        );
        conn.execute(
            "UPDATE journal SET json = ?1",
            [r#"{"summary":"recovered"}"#],
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&path).unwrap();
        let summary: String = store
            .conn
            .query_row("SELECT summary FROM journal", [], |row| row.get(0))
            .unwrap();
        assert_eq!(summary, "recovered");
        assert_eq!(store.max_journal_seq(&ProjectId::from("p")).unwrap(), 7);
    }
    fn search_entry(seq: u64, summary: &str) -> JournalEntry {
        serde_json::from_value(serde_json::json!({
            "project":"search", "seq":seq, "at":Utc::now(), "agent_name":"writer",
            "kind":"note", "summary":summary, "summary_source":"explicit"
        }))
        .unwrap()
    }

    #[test]
    fn empty_and_punctuation_searches_agree_with_and_without_fts() {
        let mut store = Store::in_memory().unwrap();
        store
            .append_journal(&search_entry(1, "100% ... _ done"))
            .unwrap();
        store
            .append_journal(&search_entry(2, "ordinary text"))
            .unwrap();
        for term in ["", "  ", "%", "...", "_", "!"] {
            let mut query = JournalQuery::new(ProjectId::from("search"), 50);
            query.grep = Some(term.into());
            let indexed = store.journal(&query).unwrap();
            store.fts = false;
            let fallback = store.journal(&query).unwrap();
            store.fts = true;
            assert_eq!(indexed, fallback, "{term:?}");
            if term.trim().is_empty() {
                assert_eq!(indexed.len(), 2);
            }
        }
    }

    #[test]
    fn fts_rebuilds_after_fallback_writes_deletions_and_missing_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        store
            .append_journal(&search_entry(1, "old searchable"))
            .unwrap();
        let store = store.without_fts();
        store.prune_journal(&ProjectId::from("search"), 2).unwrap();
        store
            .append_journal(&search_entry(2, "new searchable"))
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let mut query = JournalQuery::new(ProjectId::from("search"), 50);
        query.grep = Some("searchable".into());
        assert_eq!(
            store
                .journal(&query)
                .unwrap()
                .iter()
                .map(|e| e.seq)
                .collect::<Vec<_>>(),
            [2]
        );
        // Losing an index while retaining the completion marker also rebuilds.
        store.conn.execute("DROP TABLE journal_fts", []).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store
                .journal(&query)
                .unwrap()
                .iter()
                .map(|e| e.seq)
                .collect::<Vec<_>>(),
            [2]
        );
    }

    #[test]
    fn changes_page_downward_with_before_seq() {
        use agentdocker_core::{Attribution, Change, ChangeKind, ProjectId};
        let store = Store::in_memory().unwrap();
        let project = ProjectId::from("p1");
        for i in 0..5 {
            store
                .append_change(&Change {
                    seq: 0,
                    project: project.clone(),
                    checkout: None,
                    worktree: None,
                    path: format!("f{i}").into(),
                    kind: ChangeKind::Modified,
                    at: Utc::now(),
                    by: Attribution::External,
                    head: None,
                })
                .unwrap();
        }
        let page = |before: Option<u64>, limit: usize| {
            store
                .changes(&ChangesQuery {
                    project: project.clone(),
                    since_seq: None,
                    path: None,
                    agent: None,
                    limit,
                    after: None,
                    before_seq: before,
                })
                .unwrap()
                .into_iter()
                .map(|c| c.seq)
                .collect::<Vec<_>>()
        };
        let newest = page(None, 2);
        assert_eq!(newest.len(), 2);
        let older = page(Some(newest[0]), 2);
        assert_eq!(older.len(), 2);
        assert!(older.iter().all(|s| *s < newest[0]));
        let oldest = page(Some(older[0]), 2);
        assert_eq!(oldest.len(), 1);
        assert!(page(Some(oldest[0]), 2).is_empty());
    }
}
