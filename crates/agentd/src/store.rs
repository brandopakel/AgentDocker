//! SQLite-backed durable state.
//!
//! The in-memory structures in [`crate::daemon::Daemon`] are the source of
//! truth for reads; every mutation is written through here so a restart can
//! rebuild them. Records are stored as JSON blobs beside the few columns
//! needed for lookups, so adding a field to a core type never needs a
//! migration — only a changed meaning does.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use agentdocker_core::session::{Transfer, TransferState};
use agentdocker_core::{
    AgentId, AgentRecord, ArchivedMessage, Change, Channel, ConversationId, Envelope, Event,
    EventKind, JournalEntry, JournalKind, Lease, LeaseId, MessageId, ProjectId, ReadCursor,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) mod event_replay;
pub(crate) mod reconcile;
pub(crate) mod usage;
pub(crate) use event_replay::EventReplay;

// v9 retains pending questions. v10 retains addressed messages while subscribed
// and refuses inbox overflow. v11 adds durable identity redirects; older daemons
// would route former IDs incorrectly and must not open repaired state.
// v12 reserves managed Codex inbox delivery for its receipt-tracking bridge;
// older daemons would let legacy hooks consume the same pending input.
// v14 preserves event high-water sequences through complete history pruning.
// Older daemons can reuse sequence numbers and cannot serve checked cursors.
// v15 stores complete file-change review presentations in pending questions.
// v16 stores concrete, turn-scoped permission review presentations.
// v17 retains independent session-owner identity; v18 retains provider blocks.
// v19 retains input bindings and legacy offers on the agent record: an older
// daemon would not know a queue is a bound controller's and would drain it.
// Schema 22 adds a durable receiver-upgrade intent to bound-controller state.
// Older readers reject that field, so a downgrade must not open this database.
/// What a card's transition writes besides the card: see
/// [`Store::task_transition`].
pub struct TaskTransition<'a> {
    /// The holder whose liveness the transition records, if any.
    pub holder: Option<&'a AgentRecord>,
    /// The lease taken or renewed.
    pub claimed: Option<&'a Lease>,
    /// The leases ended.
    pub released: &'a [LeaseId],
    /// The document kind and id.
    pub kind: &'a str,
    pub id: &'a str,
    /// The events, in order.
    pub events: &'a [Event],
}

pub(crate) const SCHEMA_VERSION: i64 = 23;

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
CREATE TABLE IF NOT EXISTS coordinator (
    one    INTEGER PRIMARY KEY CHECK (one = 1),
    json   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id   TEXT NOT NULL UNIQUE,
    conversation TEXT NOT NULL,
    sender       TEXT NOT NULL,
    kind         TEXT NOT NULL,
    reply_to     TEXT,
    sent_at      TEXT NOT NULL,
    line         TEXT NOT NULL DEFAULT '',
    json         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_conversation ON messages (conversation, seq);
CREATE INDEX IF NOT EXISTS messages_reply ON messages (reply_to);
CREATE TABLE IF NOT EXISTS usage_samples (
    source_id TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    at TEXT NOT NULL,
    contribution TEXT
);
CREATE INDEX IF NOT EXISTS usage_samples_at ON usage_samples (at);
CREATE TABLE IF NOT EXISTS usage_buckets (
    key TEXT PRIMARY KEY,
    hour TEXT NOT NULL,
    agent TEXT,
    project TEXT,
    json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS usage_buckets_scope ON usage_buckets (hour, project, agent);
CREATE TABLE IF NOT EXISTS usage_baselines (
    key TEXT PRIMARY KEY,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_files (
    key TEXT PRIMARY KEY,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_gaps (
    key TEXT PRIMARY KEY,
    since TEXT,
    until TEXT NOT NULL,
    runtime TEXT,
    session TEXT,
    reason TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS read_cursors (
    reader       TEXT NOT NULL,
    conversation TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    updated_at   TEXT NOT NULL,
    PRIMARY KEY (reader, conversation)
) WITHOUT ROWID;
";

/// Full-text search over journal summaries. Contentless: the text lives in
/// the journal row, the index only maps terms to `journal.id`.
const JOURNAL_FTS: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS journal_fts USING fts5(summary, content='', contentless_delete=1)";
/// The same over archived message lines, mapping terms to `messages.seq`.
const MESSAGES_FTS: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(line, content='', contentless_delete=1)";
/// Archived messages kept per conversation, whatever the retention window.
pub const CONVERSATION_CAP: usize = 5_000;

pub struct Store {
    conn: Connection,
    /// Whether the SQLite build gave us FTS5; `--grep` falls back to LIKE.
    fts: bool,
    /// The recorded version a pending open found, whose data migrations
    /// wait for the acceptance transaction; none once they have run.
    pending_from: std::cell::Cell<Option<i64>>,
    /// Whether the message index is usable: off from the first index error
    /// until restart, when the bootstrap rebuilds it, so search falls back
    /// to LIKE at once rather than read an incomplete index.
    messages_fts: std::cell::Cell<bool>,
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
    /// Restore intent, replacement protection and its replay evidence are one
    /// recoverable transition. Keep the point until the new process is recorded.
    pub fn prepare_restore<T: serde::Serialize>(
        &self,
        record: &AgentRecord,
        point: &T,
        leases: &[Lease],
        events: &[Event],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.put_document("restore_point", record.id.as_str(), point)?;
        self.upsert_agent(record)?;
        for lease in leases {
            self.upsert_lease(lease)?;
        }
        for event in events {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Publish a lifecycle status and its replay evidence atomically.
    pub fn agent_transition(&self, record: &AgentRecord, event: &Event) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.upsert_agent(record)?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// Native exit, released protection, closed channels and replay are one commit.
    pub fn agent_exit(
        &self,
        record: &AgentRecord,
        leases: &[LeaseId],
        journal: &[JournalEntry],
        channels: &[agentdocker_core::channel::Channel],
        events: &[Event],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.upsert_agent(record)?;
        for lease in leases {
            self.delete_lease(lease)?;
        }
        for entry in journal {
            self.insert_journal(entry)?;
        }
        for channel in channels {
            self.put_document("channel", channel.id.as_str(), channel)?;
        }
        for event in events {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn finish_restore(&self, record: &AgentRecord, event: &Event) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.upsert_agent(record)?;
        self.append_event(event)?;
        self.delete_document("restore_point", record.id.as_str())?;
        tx.commit()?;
        Ok(())
    }

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

    /// The documents of one kind whose id starts with `prefix`, at most
    /// `limit`: a lookup by a unique prefix asks for two, to tell one
    /// match from several without reading every card.
    pub fn documents_with_prefix<T: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<T>> {
        let mut stmt = self.conn.prepare(
            "SELECT json FROM documents WHERE kind=?1 AND substr(id, 1, ?2) = ?3 ORDER BY id LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                kind,
                i64::try_from(prefix.chars().count()).unwrap_or(i64::MAX),
                prefix,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    /// One page of the board: `task` documents for a project (or every
    /// project), in a column (or every column), without the archived
    /// ones unless asked — Backlog to Done, oldest first within a
    /// column — from `offset`, at most `limit` cards and about `bytes`
    /// of them (a page holds at least one), and whether more follow.
    /// Read as a page so a board of long cards never fills a frame or
    /// holds the lock.
    pub fn tasks_page(
        &self,
        project: Option<&str>,
        column: Option<&str>,
        archived: bool,
        offset: usize,
        limit: usize,
        bytes: usize,
    ) -> Result<(Vec<agentdocker_core::Task>, bool)> {
        let mut stmt = self.conn.prepare(
            "SELECT json FROM documents WHERE kind='task'
             AND (?1 IS NULL OR json_extract(json, '$.project') = ?1)
             AND (?2 IS NULL OR json_extract(json, '$.column') = ?2)
             AND (?3 OR json_extract(json, '$.archived_at') IS NULL)
             ORDER BY CASE json_extract(json, '$.column')
                 WHEN 'backlog' THEN 0 WHEN 'ready' THEN 1 WHEN 'in_progress' THEN 2
                 WHEN 'review' THEN 3 ELSE 4 END,
               json_extract(json, '$.created_at'), id
             LIMIT ?4 OFFSET ?5",
        )?;
        let page = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        let skip = i64::try_from(offset).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![project, column, archived, page, skip], |row| {
            row.get::<_, String>(0)
        })?;
        let mut tasks = Vec::new();
        let mut more = false;
        let mut size = 0usize;
        for row in rows {
            let json = row?;
            if tasks.len() >= limit || (!tasks.is_empty() && size + json.len() > bytes) {
                more = true;
                break;
            }
            size += json.len();
            tasks.push(serde_json::from_str(&json)?);
        }
        Ok((tasks, more))
    }

    /// A card's transition and the leases it moves, as one commit: the
    /// card as it now reads, the lease a pull or hand took (with its
    /// holder's liveness), the leases a release, hand or archive ended,
    /// and the events for all of it — a lease without its card, or a
    /// card without its lease, is what two commits could leave behind.
    pub fn task_transition<T: serde::Serialize + ?Sized>(
        &self,
        transition: &TaskTransition<'_>,
        value: &T,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        if let Some(holder) = transition.holder {
            self.upsert_agent(holder)?;
        }
        if let Some(lease) = transition.claimed {
            self.upsert_lease(lease)?;
        }
        for lease in transition.released {
            self.delete_lease(lease)?;
        }
        self.put_document(transition.kind, transition.id, value)?;
        for event in transition.events {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(())
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
    /// Forget checkpoints and the handoff bundles carrying them, with the
    /// event that says so, in one transaction: either the rows and the
    /// announcement both land, or neither does.
    pub fn delete_checkpoints_with_event(&self, ids: &[String], event: &Event) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            self.delete_document("checkpoint", id)?;
            self.delete_document("handoff", id)?;
        }
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// Forget one document.
    pub fn delete_document(&self, kind: &str, id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM documents WHERE kind = ?1 AND id = ?2",
            params![kind, id],
        )?;
        Ok(())
    }

    /// Pruning a set of documents and its replay evidence is one commit.
    pub fn delete_documents_with_event(
        &self,
        kind: &str,
        ids: &[String],
        event: &Event,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            self.delete_document(kind, id)?;
        }
        self.append_event(event)?;
        tx.commit()?;
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
    pub(crate) fn reject_agent_writes_for_test(&self) {
        self.conn
            .execute_batch(
                "CREATE TEMP TRIGGER reject_agent_write BEFORE INSERT ON agents
            BEGIN SELECT RAISE(FAIL, 'injected agent write failure'); END;",
            )
            .unwrap();
    }

    #[cfg(test)]
    pub(crate) fn reject_lease_change_for_test(&self, operation: &str) {
        assert!(matches!(operation, "INSERT" | "DELETE"));
        self.conn
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER reject_lease_change BEFORE {operation} ON leases
             BEGIN SELECT RAISE(FAIL, 'injected lease write failure'); END;"
            ))
            .unwrap();
    }

    #[cfg(test)]
    pub(crate) fn reject_writes_for_test(&self) {
        self.conn.execute_batch("PRAGMA query_only=ON").unwrap();
    }

    /// Fail only the event insert, leaving the agent row writable.
    ///
    /// The distinction is the whole point: a sequence that wrote the row
    /// and then the event would take the row and lose the event, and
    /// memory would move on believing both had landed. One transaction
    /// takes neither. Rejecting every write cannot tell those apart.
    #[cfg(test)]
    pub(crate) fn reject_session_binding_event_for_test(&self) {
        self.conn
            .execute_batch(
                "CREATE TEMP TRIGGER reject_session_binding
            BEFORE INSERT ON events WHEN json_extract(NEW.json, '$.kind.event') = 'agent_session_bound'
            BEGIN SELECT RAISE(FAIL, 'injected session binding event failure'); END;",
            )
            .unwrap();
    }

    #[cfg(test)]
    pub(crate) fn reject_validation_finish_for_test(&self) {
        self.conn.execute_batch("CREATE TEMP TRIGGER reject_validation_finish
            BEFORE INSERT ON events WHEN json_extract(NEW.json, '$.kind.event') = 'validation_finished'
            BEGIN SELECT RAISE(FAIL, 'injected validation event failure'); END;").unwrap();
    }

    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, true)
    }

    /// Open as a successor that has not yet accepted coordination: the
    /// schema is brought forward (every migration adds; none rewrites),
    /// but the recorded version is left as the predecessor's until
    /// [`Store::settle_transfer`] accepts, in the same transaction. An
    /// aborted takeover therefore leaves a database the predecessor still
    /// opens.
    pub fn open_pending(path: &Path) -> Result<Self> {
        Self::open_with(path, false)
    }

    fn open_with(path: &Path, bump_version: bool) -> Result<Self> {
        crate::initialize_storage_platform()?;
        // Secure the database before SQLite can create a journal/WAL. Existing
        // companion files are checked without following links as well.
        agentdocker_host::dirs::private_file(path, true, false)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            let companion = PathBuf::from(name);
            match std::fs::symlink_metadata(&companion) {
                Ok(_) => {
                    agentdocker_host::dirs::private_file(&companion, false, false)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open state database {}", path.display()))?;
        Self::init(conn, bump_version)
    }

    /// A throwaway database for tests.
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::init(crate::sqlite_fixture::in_memory()?, true)
    }

    /// Behave as if the SQLite build lacked FTS5, to exercise the fallback.
    #[cfg(test)]
    pub fn without_fts(mut self) -> Self {
        self.fts = false;
        self
    }

    /// One search by the literal path, whatever the index says.
    #[cfg(test)]
    fn without_fts_search(&self, query: &str) -> Result<Vec<ArchivedMessage>> {
        let indexed = self.messages_fts.replace(false);
        let found = self.search_messages(query, None, None, 50);
        self.messages_fts.set(indexed);
        found
    }

    /// What an older database's rows must become for this build to read
    /// them as it does its own: idempotent, and only ever run with the
    /// version bump that makes them this build's.
    fn migrate_data(conn: &Connection, found: i64) -> Result<()> {
        if found < 19 {
            Self::offer_queued(conn, |_| true)?;
        }
        if found < 20 {
            // A v19 daemon could hand a synchronous ask its answer and
            // leave the row queued unrecorded; a v20 daemon says
            // answers_routed and would deliver it as fresh input.
            Self::offer_queued(conn, |envelope| envelope.reply_to.is_some())?;
        }
        if found < 21 {
            // Archive migration belongs to the same transaction as acceptance
            // and the schema bump, so an aborted successor leaves no history.
            Self::backfill_archive(conn)?;
        }
        // v22 adds the receiver upgrade intent on a binding; v23 adds the
        // `pause` document, a person's hold on a project's agents. Neither
        // rewrites a row: the bump is so an older daemon refuses the
        // database rather than open it and quietly not hold anyone.
        Ok(())
    }

    fn init(conn: Connection, bump_version: bool) -> Result<Self> {
        let mut pending_from = None;
        // Check compatibility before DDL or journal pragmas mutate the file.
        let has_meta: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')",
            [],
            |row| row.get(0),
        )?;
        let version: Option<String> = if has_meta {
            conn.query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?
        } else {
            None
        };
        if let Some(raw) = &version {
            anyhow::ensure!(
                raw.parse::<i64>()
                    .is_ok_and(|found| (1..=SCHEMA_VERSION).contains(&found)),
                "state database has schema version {raw:?}; this build expects {SCHEMA_VERSION}"
            );
        }

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

        match version.as_deref().map(str::parse::<i64>) {
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                    params![SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(Ok(found)) if found == SCHEMA_VERSION => {}
            Some(Ok(found)) if (1..SCHEMA_VERSION).contains(&found) => {
                // v2 adds stopping status and physical lease identities; v3
                // records dedicated process groups. Legacy groups default to
                // None. v4 distinguishes container lifetime from host PIDs.
                // The daemon maps legacy file keys idempotently on load.
                // Data migrations rewrite what rows mean, so they land with
                // the version that gives them that meaning: now, or for a
                // pending open in the acceptance transaction, so an aborted
                // takeover leaves the rows as the predecessor wrote them.
                if bump_version {
                    let tx = conn.unchecked_transaction()?;
                    Self::migrate_data(&conn, found)?;
                    conn.execute(
                        "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                        params![SCHEMA_VERSION.to_string()],
                    )?;
                    tx.commit()?;
                } else {
                    pending_from = Some(found);
                }
            }
            Some(other) => anyhow::bail!(
                "state database has schema version {other:?}; this build expects {SCHEMA_VERSION}"
            ),
        }
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('event_log_id', lower(hex(randomblob(16))))",
            [],
        )?;
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
        if fts {
            let had_messages_fts: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='messages_fts')",
                [],
                |row| row.get(0),
            )?;
            conn.execute_batch(MESSAGES_FTS)?;
            let complete: Option<String> = conn
                .query_row(
                    "SELECT value FROM meta WHERE key='messages_fts_complete'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            // The marker is written inside the archive's own transactions,
            // so a rollback can restore it after the index was already
            // skipped; the counts say whether it is to be believed.
            let counts_agree = || -> bool {
                let archived: i64 = conn
                    .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
                    .unwrap_or(-1);
                let indexed: i64 = conn
                    .query_row("SELECT COUNT(*) FROM messages_fts", [], |row| row.get(0))
                    .unwrap_or(-2);
                archived == indexed
            };
            if !had_messages_fts || complete.as_deref() != Some("1") || !counts_agree() {
                let tx = conn.unchecked_transaction()?;
                conn.execute(
                    "INSERT INTO messages_fts(messages_fts) VALUES('delete-all')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO messages_fts(rowid, line) SELECT seq, line FROM messages",
                    [],
                )?;
                conn.execute(
                    "INSERT OR REPLACE INTO meta(key, value) VALUES('messages_fts_complete', '1')",
                    [],
                )?;
                tx.commit()?;
            }
        }
        Ok(Self {
            messages_fts: std::cell::Cell::new(fts),
            conn,
            fts,
            pending_from: std::cell::Cell::new(pending_from),
        })
    }

    // ----- agents ---------------------------------------------------------

    /// A provider session's new record joins the prior record of its
    /// thread: the prior row takes the new process and binding, the
    /// caller's queued rows move to it, the caller's row leaves and its
    /// id becomes an alias, all with the event, in one transaction.
    pub fn resume_input(
        &self,
        canonical: &AgentRecord,
        alias: &agentdocker_core::identity::AgentAlias,
        event: &Event,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute(
            "UPDATE inbox SET agent=?1 WHERE agent=?2",
            params![alias.canonical.as_str(), alias.retired.as_str()],
        )?;
        self.conn.execute(
            "DELETE FROM journal_cursors WHERE agent=?1",
            [alias.retired.as_str()],
        )?;
        self.upsert_agent(canonical)?;
        self.conn
            .execute("DELETE FROM agents WHERE id=?1", [alias.retired.as_str()])?;
        self.put_document("identity_alias", alias.retired.as_str(), alias)?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// Mark queued messages as offered at the upgrade, so a controller
    /// that binds afterwards gets them as uncertain to reconcile rather
    /// than as new input to submit, and a binding that stands has them
    /// in its uncertain set. Before v19 nothing recorded which queued
    /// messages a hook or MCP read had already put in front of a model
    /// (every row); before v20 a synchronous ask could return its answer
    /// and leave the row queued unrecorded (every correlated reply).
    fn offer_queued(conn: &Connection, offered: impl Fn(&Envelope) -> bool) -> Result<()> {
        let now = Utc::now();
        let mut agents = conn.prepare("SELECT id, json FROM agents")?;
        let rows: Vec<(String, String)> = agents
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (id, json) in rows {
            let mut record: AgentRecord = match serde_json::from_str(&json) {
                Ok(record) => record,
                Err(_) => continue,
            };
            if Self::offer_queued_to(conn, &mut record, &offered, now)? {
                conn.execute(
                    "UPDATE agents SET json = ?1 WHERE id = ?2",
                    params![serde_json::to_string(&record)?, id],
                )?;
            }
        }
        Ok(())
    }

    /// Mark one record's queued messages that `offered` picks as offered,
    /// in the record only; whether anything changed.
    fn offer_queued_to(
        conn: &Connection,
        record: &mut AgentRecord,
        offered: &impl Fn(&Envelope) -> bool,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let mut queued = conn.prepare("SELECT json FROM inbox WHERE agent = ?1")?;
        let messages: Vec<MessageId> = queued
            .query_map(params![record.id.as_str()], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|json| serde_json::from_str::<Envelope>(&json).ok())
            .filter(|envelope| offered(envelope))
            .map(|envelope| envelope.id)
            .collect();
        if messages.is_empty() {
            return Ok(false);
        }
        for message in messages {
            record.legacy_offers.entry(message.clone()).or_insert(now);
            if let Some(binding) = record.input_binding.as_mut()
                && !binding.uncertain.contains(&message)
            {
                binding.uncertain.push(message);
            }
        }
        Ok(true)
    }

    /// What the pending data migrations would make of one record, in
    /// memory only: a successor reads its registry through this before it
    /// accepts, so the rows it serves and later writes back already carry
    /// the offers the acceptance records.
    fn project_pending(&self, record: &mut AgentRecord) -> Result<()> {
        let Some(found) = self.pending_from.get() else {
            return Ok(());
        };
        let now = Utc::now();
        if found < 19 {
            Self::offer_queued_to(&self.conn, record, &|_| true, now)?;
        }
        if found < 20 {
            Self::offer_queued_to(&self.conn, record, &|e| e.reply_to.is_some(), now)?;
        }
        Ok(())
    }

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

    /// Forget an agent, its old-ID routes and queued work in one transition.
    pub fn delete_agent(&self, id: &AgentId, event: &Event) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM documents WHERE kind = 'identity_alias'
             AND json_extract(json, '$.canonical') = ?1",
            params![id.as_str()],
        )?;
        tx.execute("DELETE FROM inbox WHERE agent = ?1", params![id.as_str()])?;
        tx.execute(
            "DELETE FROM journal_cursors WHERE agent = ?1",
            params![id.as_str()],
        )?;
        tx.execute("DELETE FROM agents WHERE id = ?1", params![id.as_str()])?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_agents(&self) -> Result<Vec<AgentRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT json FROM agents ORDER BY created_at, id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut records: Vec<AgentRecord> = rows
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_>>()?;
        for record in &mut records {
            self.project_pending(record)?;
        }
        Ok(records)
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

    /// Liveness, an optional claimed/renewed lease, and its replay event are
    /// one durable admission result. Conflicts update liveness without a lease.
    pub fn lease_activity(
        &self,
        agent: &AgentRecord,
        lease: Option<&Lease>,
        event: Option<&Event>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.upsert_agent(agent)?;
        if let Some(lease) = lease {
            self.upsert_lease(lease)?;
        }
        if let Some(event) = event {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_leases(&self) -> Result<Vec<Lease>> {
        let mut stmt = self.conn.prepare("SELECT json FROM leases")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    // ----- inboxes --------------------------------------------------------

    /// Message routing, sender activity and question lifecycle are one durable
    /// transition. Nothing may reach memory, live subscribers or notifications
    /// before this commits, including a broadcast's partially written inboxes.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn publish_message(
        &self,
        message: &Envelope,
        recipients: &[AgentId],
        capacity: usize,
        sender: Option<&AgentRecord>,
        question: Option<&agentdocker_core::Question>,
        closed: Option<&agentdocker_core::MessageId>,
        events: &[Event],
    ) -> Result<()> {
        self.publish_message_with_channel(
            message, recipients, capacity, sender, question, closed, events, None, None,
        )
    }

    /// A channel close/review and all of its message/journal effects are one
    /// transaction; a refused ancillary write cannot leave an applied action.
    #[allow(clippy::too_many_arguments)]
    pub fn publish_message_with_channel(
        &self,
        message: &Envelope,
        recipients: &[AgentId],
        capacity: usize,
        sender: Option<&AgentRecord>,
        question: Option<&agentdocker_core::Question>,
        closed: Option<&agentdocker_core::MessageId>,
        events: &[Event],
        channel: Option<(&Channel, Option<&JournalEntry>)>,
        document: Option<(&str, &str, Option<&serde_json::Value>)>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for recipient in recipients {
            self.insert_inbox(recipient, message, capacity)?;
        }
        match document {
            Some((kind, id, Some(value))) => self.put_document(kind, id, value)?,
            Some((kind, id, None)) => self.delete_document(kind, id)?,
            None => {}
        }
        // The archive is written beside the queues, never instead of them:
        // a queue is what a recipient has not taken, the archive is what
        // was said in the conversation.
        if let Some(conversation) = ConversationId::of(message) {
            self.archive_message(&self.conn, message, &conversation)?;
        }
        if let Some(sender) = sender {
            self.upsert_agent(sender)?;
        }
        if let Some(question) = question {
            self.put_document("question", question.id.as_str(), question)?;
        }
        if let Some(closed) = closed {
            self.delete_document("question", closed.as_str())?;
        }
        if let Some((channel, journal)) = channel {
            self.put_document("channel", channel.id.as_str(), channel)?;
            if let Some(entry) = journal {
                self.insert_journal(entry)?;
            }
        }
        for event in events {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn close_questions(
        &self,
        questions: &[agentdocker_core::MessageId],
        events: &[Event],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for question in questions {
            self.delete_document("question", question.as_str())?;
        }
        for event in events {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub fn enqueue(&self, agent: &AgentId, message: &Envelope, capacity: usize) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.insert_inbox(agent, message, capacity)?;
        tx.commit()?;
        Ok(())
    }

    // ----- conversations --------------------------------------------------

    fn archive_message(
        &self,
        conn: &Connection,
        message: &Envelope,
        conversation: &ConversationId,
    ) -> Result<()> {
        let line = agentdocker_core::conversation::line_of(message);
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO messages (message_id, conversation, sender, kind, reply_to, sent_at, line, json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.id.as_str(),
                conversation.as_str(),
                message.from,
                message.kind,
                message.reply_to.as_ref().map(|id| id.as_str()),
                message.sent_at.to_rfc3339(),
                line,
                serde_json::to_string(message)?,
            ],
        )?;
        if inserted == 1 && self.messages_fts.get() {
            let seq = conn.last_insert_rowid();
            if let Err(err) = conn.execute(
                "INSERT INTO messages_fts (rowid, line) VALUES (?1, ?2)",
                params![seq, line],
            ) {
                tracing::warn!(%err, "messages_fts insert failed; search falls back to LIKE until restart");
                self.messages_fts.set(false);
            }
        }
        if inserted == 1 && !self.messages_fts.get() {
            // A prior transaction may have rolled its marker deletion back
            // while leaving the in-memory index disabled. Every committed
            // unindexed mutation must commit an incomplete marker too.
            conn.execute("DELETE FROM meta WHERE key='messages_fts_complete'", [])?;
        }
        Ok(())
    }

    /// Everything still queued becomes the start of the archive, once, by
    /// message id: nothing acknowledged earlier exists anywhere to keep.
    fn backfill_archive(conn: &Connection) -> Result<()> {
        let mut rows = conn.prepare("SELECT json FROM inbox ORDER BY seq")?;
        let envelopes: Vec<Envelope> = rows
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|json| serde_json::from_str(&json).ok())
            .collect();
        for envelope in envelopes {
            if let Some(conversation) = ConversationId::of(&envelope) {
                conn.execute(
                    "INSERT OR IGNORE INTO messages (message_id, conversation, sender, kind, reply_to, sent_at, line, json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        envelope.id.as_str(),
                        conversation.as_str(),
                        envelope.from,
                        envelope.kind,
                        envelope.reply_to.as_ref().map(|id| id.as_str()),
                        envelope.sent_at.to_rfc3339(),
                        agentdocker_core::conversation::line_of(&envelope),
                        serde_json::to_string(&envelope)?,
                    ],
                )?;
            }
        }
        conn.execute("DELETE FROM meta WHERE key='messages_fts_complete'", [])?;
        // What was queued before there were conversations is not new to
        // the person: their cursor starts at the head of every
        // conversation, so the first unread count is what arrives next,
        // not the whole past. Agents keep their queues as they were.
        let mut agents = conn.prepare("SELECT id, json FROM agents")?;
        let humans: Vec<String> = agents
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(Result::ok)
            .filter(|(_, json)| {
                serde_json::from_str::<AgentRecord>(json)
                    .is_ok_and(|r| r.spec.runtime == agentdocker_core::HUMAN_RUNTIME)
            })
            .map(|(id, _)| id)
            .collect();
        if !humans.is_empty() {
            let mut heads =
                conn.prepare("SELECT conversation, MAX(seq) FROM messages GROUP BY conversation")?;
            let heads: Vec<(String, i64)> = heads
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            let now = Utc::now().to_rfc3339();
            for human in &humans {
                for (conversation, seq) in &heads {
                    conn.execute(
                        "INSERT OR IGNORE INTO read_cursors (reader, conversation, seq, updated_at) VALUES (?1, ?2, ?3, ?4)",
                        params![human, conversation, seq, now],
                    )?;
                }
            }
        }
        Ok(())
    }

    fn archived_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArchivedMessage> {
        let seq: i64 = row.get(0)?;
        let conversation: String = row.get(1)?;
        let json: String = row.get(2)?;
        let replies: i64 = row.get(3)?;
        let envelope: Envelope = serde_json::from_str(&json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(ArchivedMessage {
            seq: u64::try_from(seq).unwrap_or_default(),
            conversation: ConversationId::from(conversation),
            envelope,
            replies: u64::try_from(replies).unwrap_or_default(),
        })
    }

    const ARCHIVED_COLUMNS: &'static str = "m.seq, m.conversation, m.json, \
        (SELECT COUNT(*) FROM messages r WHERE r.reply_to = m.message_id AND r.conversation = m.conversation)";

    /// The newest `limit` messages of a conversation before `before_seq`,
    /// oldest first, each root with its reply count.
    pub fn history(
        &self,
        conversation: &ConversationId,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<ArchivedMessage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM messages m WHERE m.conversation = ?1 AND m.seq < ?2 ORDER BY m.seq DESC LIMIT ?3",
            Self::ARCHIVED_COLUMNS
        ))?;
        let before = before_seq.map_or(i64::MAX, |s| i64::try_from(s).unwrap_or(i64::MAX));
        let mut rows: Vec<ArchivedMessage> = stmt
            .query_map(
                params![
                    conversation.as_str(),
                    before,
                    i64::try_from(limit.clamp(1, 500)).unwrap_or(500)
                ],
                Self::archived_row,
            )?
            .collect::<std::result::Result<_, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// One archived message by id, with its reply count.
    pub fn archived(&self, message: &MessageId) -> Result<Option<ArchivedMessage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM messages m WHERE m.message_id = ?1",
            Self::ARCHIVED_COLUMNS
        ))?;
        Ok(stmt
            .query_row([message.as_str()], Self::archived_row)
            .optional()?)
    }

    /// The replies threaded under a root: same conversation, oldest first,
    /// after `after_seq`, at most `limit`; page by the last seq shown.
    pub fn thread_replies(
        &self,
        root: &MessageId,
        conversation: &ConversationId,
        after_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<ArchivedMessage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM messages m WHERE m.reply_to = ?1 AND m.conversation = ?2 AND m.seq > ?3 ORDER BY m.seq LIMIT ?4",
            Self::ARCHIVED_COLUMNS
        ))?;
        Ok(stmt
            .query_map(
                params![
                    root.as_str(),
                    conversation.as_str(),
                    after_seq.map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX)),
                    i64::try_from(limit.clamp(1, 500)).unwrap_or(500)
                ],
                Self::archived_row,
            )?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// The last message of every conversation that has one.
    pub fn conversation_heads(&self) -> Result<Vec<ArchivedMessage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM messages m WHERE m.seq IN (SELECT MAX(seq) FROM messages GROUP BY conversation)",
            Self::ARCHIVED_COLUMNS
        ))?;
        Ok(stmt
            .query_map([], Self::archived_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// How many archived messages of a conversation lie past a seq, not
    /// counting the reader's own words.
    /// Messages past the cursor that none of the reader's identities
    /// sent: what a former identity of the reader said is the reader's own
    /// words too, not something waiting to be read.
    pub fn unread_after(
        &self,
        conversation: &ConversationId,
        after_seq: u64,
        readers: &[AgentId],
    ) -> Result<u64> {
        let senders = readers
            .iter()
            .map(|id| format!("'{}'", id.as_str().replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let count: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM messages WHERE conversation = ?1 AND seq > ?2 AND sender NOT IN ({senders})"
            ),
            params![
                conversation.as_str(),
                i64::try_from(after_seq).unwrap_or(i64::MAX),
            ],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or_default())
    }

    /// How many unread rows mention the reader: rows past the cursor by
    /// somebody else whose text carries `@` and one of these names, as
    /// `mentions_any` reads them. A LIKE narrows the rows to those with
    /// an `@` at all; the names are matched in Rust so `@user` does not
    /// count for `@users`.
    pub fn mentions_after(
        &self,
        conversation: &ConversationId,
        after_seq: u64,
        readers: &[AgentId],
        names: &[String],
    ) -> Result<u64> {
        if names.is_empty() {
            return Ok(0);
        }
        let senders = readers
            .iter()
            .map(|id| format!("'{}'", id.as_str().replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let mut statement = self.conn.prepare(&format!(
            "SELECT json FROM messages WHERE conversation = ?1 AND seq > ?2 AND sender NOT IN ({senders}) AND json LIKE '%@%'"
        ))?;
        let rows = statement.query_map(
            params![
                conversation.as_str(),
                i64::try_from(after_seq).unwrap_or(i64::MAX),
            ],
            |row| row.get::<_, String>(0),
        )?;
        let mut count = 0;
        for raw in rows {
            let envelope: Envelope = serde_json::from_str(&raw?)?;
            if agentdocker_core::conversation::mentions_any(
                &agentdocker_core::conversation::line_of(&envelope),
                names,
            ) {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Whether an archived row with this seq belongs to the conversation.
    pub fn seq_in_conversation(&self, conversation: &ConversationId, seq: u64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE conversation = ?1 AND seq = ?2)",
            params![
                conversation.as_str(),
                i64::try_from(seq).unwrap_or(i64::MAX)
            ],
            |row| row.get(0),
        )?)
    }

    /// Message ids of a conversation up to and including a seq.
    pub fn message_ids_through(
        &self,
        conversation: &ConversationId,
        through: u64,
    ) -> Result<Vec<MessageId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT message_id FROM messages WHERE conversation = ?1 AND seq <= ?2")?;
        Ok(stmt
            .query_map(
                params![
                    conversation.as_str(),
                    i64::try_from(through).unwrap_or(i64::MAX)
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(MessageId::from)
            .collect())
    }

    pub fn read_cursors(&self, reader: &str) -> Result<Vec<ReadCursor>> {
        let mut stmt = self
            .conn
            .prepare("SELECT conversation, seq, updated_at FROM read_cursors WHERE reader = ?1")?;
        Ok(stmt
            .query_map([reader], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(conversation, seq, at)| ReadCursor {
                reader: AgentId::from(reader.to_owned()),
                conversation: ConversationId::from(conversation),
                through: u64::try_from(seq).unwrap_or_default(),
                updated_at: DateTime::parse_from_rfc3339(&at)
                    .map(|t| t.with_timezone(&Utc))
                    .unwrap_or_default(),
            })
            .collect())
    }

    /// Move a reader's cursor forward, acknowledge the reader's queued rows
    /// the cursor now covers, and record the event, in one transaction.
    pub fn mark_read(
        &self,
        cursor: &ReadCursor,
        acknowledged: &[MessageId],
        event: &Event,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute(
            "INSERT INTO read_cursors (reader, conversation, seq, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(reader, conversation) DO UPDATE SET
                 seq = MAX(seq, excluded.seq), updated_at = excluded.updated_at",
            params![
                cursor.reader.as_str(),
                cursor.conversation.as_str(),
                i64::try_from(cursor.through).unwrap_or(i64::MAX),
                cursor.updated_at.to_rfc3339()
            ],
        )?;
        for message in acknowledged {
            self.conn.execute(
                "DELETE FROM inbox WHERE agent = ?1 AND message_id = ?2",
                params![cursor.reader.as_str(), message.as_str()],
            )?;
        }
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// Archived messages whose line matches, newest first, before a seq,
    /// within the named conversations when given.
    pub fn search_messages(
        &self,
        query: &str,
        conversations: Option<&[ConversationId]>,
        before_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<ArchivedMessage>> {
        let before = before_seq.map_or(i64::MAX, |s| i64::try_from(s).unwrap_or(i64::MAX));
        let limit = i64::try_from(limit.clamp(1, 200)).unwrap_or(200);
        let scope_clause = match conversations {
            Some([]) => " AND 0".to_owned(),
            Some(ids) => format!(
                " AND m.conversation IN ({})",
                ids.iter()
                    .map(|id| format!("'{}'", id.as_str().replace('\'', "''")))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            None => String::new(),
        };
        // A term the index cannot take literally (quotes, the LIKE
        // wildcards, a backslash, no word at all) is looked for by LIKE
        // with those characters escaped, so both paths find the same rows.
        let indexed = self.messages_fts.get()
            && query.chars().any(char::is_alphanumeric)
            && !query.chars().any(|c| matches!(c, '"' | '%' | '_' | '\\'));
        let sql = if indexed {
            format!(
                "SELECT {} FROM messages m WHERE m.seq < ?1{scope_clause} AND m.seq IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?2) ORDER BY m.seq DESC LIMIT ?3",
                Self::ARCHIVED_COLUMNS
            )
        } else {
            format!(
                "SELECT {} FROM messages m WHERE m.seq < ?1{scope_clause} AND m.line LIKE '%' || ?2 || '%' ESCAPE '\\' ORDER BY m.seq DESC LIMIT ?3",
                Self::ARCHIVED_COLUMNS
            )
        };
        let term = if indexed {
            format!("\"{query}\"")
        } else {
            query
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        };
        let rows = self.conn.prepare(&sql).and_then(|mut stmt| {
            stmt.query_map(params![before, term, limit], Self::archived_row)?
                .collect::<std::result::Result<Vec<_>, _>>()
        });
        match rows {
            Ok(rows) => Ok(rows),
            // An index that fails to answer is unusable: say so once, and
            // answer this and every later search by LIKE.
            Err(err) if indexed && self.messages_fts.get() => {
                tracing::warn!(%err, "messages_fts query failed; search falls back to LIKE until restart");
                self.messages_fts.set(false);
                // Search remains read-only during a coordinator transfer.
                // Any subsequent unindexed archive mutation clears the durable
                // completeness marker in its own transaction.
                self.search_messages(
                    query,
                    conversations,
                    before_seq,
                    usize::try_from(limit).unwrap_or(200),
                )
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Drop archived messages older than `cutoff`, and beyond the cap per
    /// conversation, within one budget of `batch` rows for the whole tick,
    /// never a conversation's last message: the head stays, so the sidebar
    /// keeps its last line and the sequence its meaning. Returns how many
    /// went.
    #[cfg(test)]
    pub fn prune_messages(
        &self,
        cutoff: Option<DateTime<Utc>>,
        cap: usize,
        batch: usize,
    ) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let removed = self.prune_messages_inner(cutoff, cap, batch)?;
        tx.commit()?;
        Ok(removed)
    }

    /// Commit a bounded prune with its exact-count event, or neither.
    pub fn prune_messages_with_event(
        &self,
        cutoff: Option<DateTime<Utc>>,
        cap: usize,
        batch: usize,
        seq: u64,
        now: DateTime<Utc>,
    ) -> Result<Option<Event>> {
        let tx = self.conn.unchecked_transaction()?;
        let removed = self.prune_messages_inner(cutoff, cap, batch)?;
        let event = (removed > 0).then(|| {
            let mut event = Event::new(EventKind::MessagesPruned { removed }, now);
            event.seq = seq;
            event
        });
        if let Some(event) = &event {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(event)
    }

    fn prune_messages_inner(
        &self,
        cutoff: Option<DateTime<Utc>>,
        cap: usize,
        batch: usize,
    ) -> Result<usize> {
        let mut budget = i64::try_from(batch).unwrap_or(i64::MAX);
        let mut removed = 0usize;
        // A row is a head when it is its conversation's newest.
        const NOT_HEAD: &str = "seq < (SELECT MAX(h.seq) FROM messages h WHERE h.conversation = messages.conversation)";
        if let Some(cutoff) = cutoff
            && budget > 0
        {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT seq FROM messages WHERE sent_at < ?1 AND {NOT_HEAD} ORDER BY seq LIMIT ?2"
            ))?;
            let seqs: Vec<i64> = stmt
                .query_map(params![cutoff.to_rfc3339(), budget], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            let went = self.delete_archived(&seqs)?;
            removed += went;
            budget -= i64::try_from(went).unwrap_or(i64::MAX);
        }
        let cap_i = i64::try_from(cap).unwrap_or(i64::MAX);
        let mut over = self.conn.prepare(
            "SELECT conversation, COUNT(*) FROM messages GROUP BY conversation HAVING COUNT(*) > ?1 ORDER BY conversation",
        )?;
        let crowded: Vec<(String, i64)> = over
            .query_map([cap_i], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        drop(over);
        for (conversation, count) in crowded {
            if budget <= 0 {
                break;
            }
            let excess = (count - cap_i).min(budget);
            let mut stmt = self.conn.prepare(&format!(
                "SELECT seq FROM messages WHERE conversation = ?1 AND {NOT_HEAD} ORDER BY seq LIMIT ?2"
            ))?;
            let seqs: Vec<i64> = stmt
                .query_map(params![conversation, excess], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            let went = self.delete_archived(&seqs)?;
            removed += went;
            budget -= i64::try_from(went).unwrap_or(i64::MAX);
        }
        Ok(removed)
    }

    fn delete_archived(&self, seqs: &[i64]) -> Result<usize> {
        let mut removed = 0;
        for seq in seqs {
            removed += self
                .conn
                .execute("DELETE FROM messages WHERE seq = ?1", [seq])?;
            if self.messages_fts.get()
                && let Err(err) = self
                    .conn
                    .execute("DELETE FROM messages_fts WHERE rowid = ?1", [seq])
            {
                tracing::warn!(%err, "messages_fts delete failed; search falls back to LIKE until restart");
                self.messages_fts.set(false);
            }
        }
        if removed > 0 && !self.messages_fts.get() {
            self.conn
                .execute("DELETE FROM meta WHERE key='messages_fts_complete'", [])?;
        }
        Ok(removed)
    }

    fn insert_inbox(&self, agent: &AgentId, message: &Envelope, capacity: usize) -> Result<()> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM inbox WHERE agent = ?1",
            [agent.as_str()],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            usize::try_from(count)? < capacity,
            "recipient inbox is full"
        );
        self.conn.execute(
            "INSERT INTO inbox (agent, message_id, json) VALUES (?1, ?2, ?3)",
            params![
                agent.as_str(),
                message.id.as_str(),
                serde_json::to_string(message)?
            ],
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
            sql.push_str(&identity_filter("by_agent", args.len()));
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
            sql.push_str(&identity_filter("agent", args.len()));
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

    /// The boundary one retention pass may prune to for a project: walking
    /// the oldest `batch` entries in sequence order, one past the last entry
    /// before the first one written at or after `cutoff`. Stopping at the
    /// first fresh entry, rather than taking the highest expired sequence,
    /// means a clock that moved backwards can never make an unexpired entry
    /// disappear with the expired ones around it. `None` when the oldest
    /// entry is still inside the window, which costs one index probe.
    pub fn journal_retention_boundary(
        &self,
        project: &ProjectId,
        cutoff: DateTime<Utc>,
        batch: usize,
    ) -> Result<Option<u64>> {
        let cutoff = cutoff.to_rfc3339();
        let mut stmt = self
            .conn
            .prepare("SELECT seq, at FROM journal WHERE project = ?1 ORDER BY seq LIMIT ?2")?;
        let rows = stmt.query_map(
            params![project.as_str(), i64::try_from(batch).unwrap_or(i64::MAX)],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut boundary = None;
        for row in rows {
            let (seq, at) = row?;
            if at >= cutoff {
                break;
            }
            boundary = u64::try_from(seq).ok().map(|seq| seq + 1);
        }
        Ok(boundary)
    }

    /// Every project that has journal entries, in a stable order so a
    /// retention pass visits each exactly once per tick.
    pub fn journal_projects(&self) -> Result<Vec<ProjectId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT project FROM journal_heads ORDER BY project")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(ProjectId::from(row?.as_str()))).collect()
    }

    /// Backdate an entry so retention tests need not wait for real time.
    #[cfg(test)]
    pub fn age_journal_for_test(&self, project: &ProjectId, seq: u64, at: DateTime<Utc>) {
        self.conn
            .execute(
                "UPDATE journal SET at = ?3 WHERE project = ?1 AND seq = ?2",
                params![
                    project.as_str(),
                    i64::try_from(seq).unwrap_or(i64::MAX),
                    at.to_rfc3339()
                ],
            )
            .unwrap();
    }

    /// Give freed pages back to the filesystem. Returns the database size
    /// before and after. Runs outside any transaction, as SQLite requires.
    pub fn vacuum(&self) -> Result<(u64, u64)> {
        let size = || -> Result<u64> {
            let pages: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            let page: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
            Ok(u64::try_from(pages.saturating_mul(page)).unwrap_or(0))
        };
        let before = size()?;
        self.conn.execute_batch("VACUUM")?;
        Ok((before, size()?))
    }

    // ----- coordinator transfer -----------------------------------------

    /// The transfer row, if any daemon ever offered one.
    pub fn transfer(&self) -> Result<Option<Transfer>> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM coordinator WHERE one = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        json.map(|text| Ok(serde_json::from_str(&text)?))
            .transpose()
    }

    /// Record an offer. Refused while another transfer is still offered:
    /// two successors must never be invited at once.
    pub fn offer_transfer(&self, transfer: &Transfer, event: &Event) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        if let Some(current) = self.transfer()?
            && current.state == TransferState::Offered
            && current.id != transfer.id
        {
            return Ok(false);
        }
        self.conn.execute(
            "INSERT INTO coordinator (one, json) VALUES (1, ?1) ON CONFLICT(one) DO UPDATE SET json = excluded.json",
            params![serde_json::to_string(transfer)?],
        )?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(true)
    }

    /// Rewrite an offer's successor pid while it is still offered, with
    /// the event that says so in the same transaction.
    pub fn readdress_transfer(&self, id: &str, successor_pid: u32, event: &Event) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let Some(mut current) = self.transfer()? else {
            return Ok(false);
        };
        if current.id != id || current.state != TransferState::Offered {
            return Ok(false);
        }
        current.successor_pid = Some(successor_pid);
        self.conn.execute(
            "UPDATE coordinator SET json = ?1 WHERE one = 1",
            params![serde_json::to_string(&current)?],
        )?;
        self.append_event(event)?;
        tx.commit()?;
        Ok(true)
    }

    /// The schema this build writes: the compiled version, which open
    /// brings the database to, or refuses.
    pub fn schema_version(&self) -> i64 {
        SCHEMA_VERSION
    }

    /// The schema version the database records right now; behind
    /// [`Store::schema_version`] only for a successor that has opened
    /// pending and not yet accepted.
    pub fn recorded_schema_version(&self) -> Result<i64> {
        let raw: String = self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        Ok(raw.parse()?)
    }

    /// Move the transfer `id` from `Offered` to `to`, only if it is still
    /// offered and, when `successor_pid` is given, offered to that pid. The
    /// first write a successor makes is this accept; a predecessor taking
    /// authority back writes the abort. Whichever lands first wins, and the
    /// other learns it did not.
    pub fn settle_transfer(
        &self,
        id: &str,
        successor_pid: Option<u32>,
        to: TransferState,
        settled_at: DateTime<Utc>,
        event: &Event,
    ) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let Some(mut current) = self.transfer()? else {
            return Ok(false);
        };
        if current.id != id || current.state != TransferState::Offered {
            return Ok(false);
        }
        if let Some(pid) = successor_pid
            && current.successor_pid != Some(pid)
        {
            return Ok(false);
        }
        current.state = to;
        current.settled_at = Some(settled_at);
        self.conn.execute(
            "UPDATE coordinator SET json = ?1 WHERE one = 1",
            params![serde_json::to_string(&current)?],
        )?;
        if to == TransferState::Accepted {
            // The database is this build's from here: a successor that
            // opened pending brings the rows and the recorded version
            // forward with the same write that makes it the coordinator.
            if let Some(found) = self.pending_from.get() {
                Self::migrate_data(&self.conn, found)?;
            }
            self.conn.execute(
                "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                params![SCHEMA_VERSION.to_string()],
            )?;
        }
        self.append_event(event)?;
        tx.commit()?;
        if to == TransferState::Accepted {
            if self.pending_from.get().is_some_and(|found| found < 21) {
                // The index was initialized before the deferred archive
                // backfill. Use complete literal search until restart rebuilds
                // it; the migration removed the durable completeness marker.
                self.messages_fts.set(false);
            }
            self.pending_from.set(None);
        }
        Ok(true)
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
                i64::try_from(event.seq).context("durable event sequence exhausted")?,
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
                .query_row(
                    "SELECT MAX(COALESCE((SELECT seq FROM sqlite_sequence WHERE name='events'), 0), COALESCE((SELECT MAX(seq) FROM events), 0))",
                    [],
                    |row| row.get(0),
                )?;
        u64::try_from(max).context("negative durable event sequence")
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

/// Include original attribution under every exact former identity. The alias
/// table is authoritative; history remains byte-for-byte as originally stored.
fn identity_filter(column: &str, parameter: usize) -> String {
    let canonical = format!(
        "COALESCE((SELECT json_extract(json, '$.canonical') FROM documents WHERE kind='identity_alias' AND id=?{parameter}), ?{parameter})"
    );
    format!(
        " AND ({column} = {canonical} OR {column} IN (SELECT id FROM documents WHERE kind='identity_alias' AND json_extract(json, '$.canonical') = {canonical}))"
    )
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

        store
            .delete_agent(
                &a.id,
                &Event::new(
                    EventKind::AgentRemoved {
                        agent: a.id.clone(),
                    },
                    Utc::now(),
                ),
            )
            .unwrap();
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
            amount: 0,
            automatic: false,
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
            amount: 0,
            automatic: false,
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
    fn full_inbox_refuses_new_work_and_preserves_every_accepted_message() {
        let store = Store::in_memory().unwrap();
        let agent = AgentId::from("a");
        for i in 0..3 {
            store.enqueue(&agent, &envelope(&i.to_string()), 3).unwrap();
        }
        assert!(store.enqueue(&agent, &envelope("refused"), 3).is_err());
        let inboxes = store.load_inboxes().unwrap();
        let texts: Vec<String> = inboxes[&agent]
            .iter()
            .map(|m| m.payload["text"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(texts, vec!["0", "1", "2"]);

        let ids = inboxes[&agent]
            .iter()
            .map(|message| message.id.clone())
            .collect::<Vec<_>>();
        let mut event = Event::new(
            EventKind::InboxAcknowledged {
                agent: agent.clone(),
                messages: ids.clone(),
            },
            Utc::now(),
        );
        event.seq = 1;
        store.ack_inbox(&agent, &ids, &event).unwrap();
        assert!(store.load_inboxes().unwrap().is_empty());
        assert_eq!(store.recent_events(1).unwrap()[0], event);
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
        let conn = crate::sqlite_fixture::in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', '999')",
            [],
        )
        .unwrap();
        assert!(Store::init(conn, true).is_err());
    }

    /// A database one version newer than this build — the next daemon
    /// wrote something this one would not honour, a project's pause among
    /// them — is refused, with both numbers in the reason, rather than
    /// opened and read as if the newer meaning were not there; the
    /// version this build writes opens, and a pause document in it is
    /// read back whole.
    #[test]
    fn a_newer_database_is_refused_and_this_versions_pause_is_kept() {
        let conn = crate::sqlite_fixture::in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![(SCHEMA_VERSION + 1).to_string()],
        )
        .unwrap();
        let error = match Store::init(conn, true) {
            Ok(_) => panic!("a newer database opened"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(&(SCHEMA_VERSION + 1).to_string())
                && error.contains(&SCHEMA_VERSION.to_string()),
            "{error}"
        );
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let pause = agentdocker_core::Pause {
            project: ProjectId::from("p"),
            by: "user".into(),
            reason: "sleeping the laptop".into(),
            at: Utc::now(),
        };
        store.put_document("pause", "p", &pause).unwrap();
        drop(store);
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let kept: Vec<agentdocker_core::Pause> = store.documents("pause", None).unwrap();
        assert_eq!(kept, vec![pause]);
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            SCHEMA_VERSION.to_string()
        );
    }

    /// A successor that opens pending brings the schema forward but not
    /// its recorded version: an aborted takeover leaves the number the
    /// predecessor wrote, and only acceptance moves it, in the same
    /// transaction that makes the successor the coordinator.
    #[test]
    fn a_pending_open_records_the_new_schema_version_only_on_acceptance() {
        use agentdocker_core::session::{Transfer, TransferState};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let conn = crate::sqlite_fixture::open(&path).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                params![(SCHEMA_VERSION - 1).to_string()],
            )
            .unwrap();
        }
        let now = Utc::now();
        let seq = std::cell::Cell::new(0_u64);
        let event = |kind: EventKind| {
            let mut event = Event::new(kind, now);
            seq.set(seq.get() + 1);
            event.seq = seq.get();
            event
        };
        let transfer = |id: &str| Transfer {
            id: id.into(),
            predecessor_pid: 1,
            successor_pid: Some(2),
            state: TransferState::Offered,
            offered_at: now,
            settled_at: None,
        };

        // Offered, opened pending, aborted: the number never moved.
        let pending = Store::open_pending(&path).unwrap();
        assert_eq!(
            pending.recorded_schema_version().unwrap(),
            SCHEMA_VERSION - 1
        );
        assert!(
            pending
                .offer_transfer(
                    &transfer("t1"),
                    &event(EventKind::DaemonTransferOffered {
                        transfer: "t1".into(),
                        successor_pid: 2
                    })
                )
                .unwrap()
        );
        assert!(
            pending
                .settle_transfer(
                    "t1",
                    None,
                    TransferState::Aborted,
                    now,
                    &event(EventKind::DaemonTransferAborted {
                        transfer: "t1".into(),
                        reason: "test".into()
                    })
                )
                .unwrap()
        );
        assert_eq!(
            pending.recorded_schema_version().unwrap(),
            SCHEMA_VERSION - 1
        );
        drop(pending);
        assert_eq!(
            Store::open_pending(&path)
                .unwrap()
                .recorded_schema_version()
                .unwrap(),
            SCHEMA_VERSION - 1,
            "reopening pending still leaves it"
        );

        // Accepted: the number comes forward with the acceptance.
        let pending = Store::open_pending(&path).unwrap();
        assert!(
            pending
                .offer_transfer(
                    &transfer("t2"),
                    &event(EventKind::DaemonTransferOffered {
                        transfer: "t2".into(),
                        successor_pid: 2
                    })
                )
                .unwrap()
        );
        assert!(
            pending
                .settle_transfer(
                    "t2",
                    Some(2),
                    TransferState::Accepted,
                    now,
                    &event(EventKind::DaemonTransferAccepted {
                        transfer: "t2".into()
                    })
                )
                .unwrap()
        );
        assert_eq!(pending.recorded_schema_version().unwrap(), SCHEMA_VERSION);
        drop(pending);
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .recorded_schema_version()
                .unwrap(),
            SCHEMA_VERSION
        );
    }

    /// A successor that opened pending leaves the rows as the predecessor
    /// wrote them: a v18 queue is not marked offered until the acceptance
    /// that makes the database this build's, and an aborted takeover
    /// leaves it unmarked for the predecessor.
    #[test]
    fn a_pending_open_defers_data_migrations_to_the_acceptance() {
        use agentdocker_core::session::{Transfer, TransferState};
        use agentdocker_core::{AgentSpec, Destination};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let now = Utc::now();
        let record = AgentRecord::new(AgentSpec::default(), false, now);
        {
            let conn = crate::sqlite_fixture::open(&path).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version', '18')",
                [],
            )
            .unwrap();
            let mut json = serde_json::to_value(&record).unwrap();
            json.as_object_mut().unwrap().remove("legacy_offers");
            json.as_object_mut().unwrap().remove("input_binding");
            conn.execute(
                "INSERT INTO agents (id, name, live, created_at, json) VALUES (?1, 'old', 1, ?2, ?3)",
                params![record.id.as_str(), now.to_rfc3339(), json.to_string()],
            )
            .unwrap();
            let envelope = Envelope::new(
                "peer",
                Destination::Agent(record.id.clone()),
                "chat",
                serde_json::json!({ "text": "old" }),
                None,
                now,
            );
            conn.execute(
                "INSERT INTO inbox (agent, message_id, json) VALUES (?1, ?2, ?3)",
                params![
                    record.id.as_str(),
                    envelope.id.as_str(),
                    serde_json::to_string(&envelope).unwrap()
                ],
            )
            .unwrap();
        }
        let offers = |store: &Store| {
            store
                .load_agents()
                .unwrap()
                .into_iter()
                .find(|a| a.id == record.id)
                .unwrap()
                .legacy_offers
                .len()
        };
        let seq = std::cell::Cell::new(0_u64);
        let event = |kind: EventKind| {
            let mut event = Event::new(kind, now);
            seq.set(seq.get() + 1);
            event.seq = seq.get();
            event
        };
        let transfer = |id: &str| Transfer {
            id: id.into(),
            predecessor_pid: 1,
            successor_pid: Some(2),
            state: TransferState::Offered,
            offered_at: now,
            settled_at: None,
        };
        let pending = Store::open_pending(&path).unwrap();
        let stored = |store: &Store| -> usize {
            let json: String = store
                .conn
                .query_row(
                    "SELECT json FROM agents WHERE id = ?1",
                    [record.id.as_str()],
                    |r| r.get(0),
                )
                .unwrap();
            serde_json::from_str::<AgentRecord>(&json)
                .unwrap()
                .legacy_offers
                .len()
        };
        assert_eq!(stored(&pending), 0, "pending: the row is as v18 wrote it");
        assert!(
            pending.conversation_heads().unwrap().is_empty(),
            "pending archive is empty"
        );
        assert_eq!(
            offers(&pending),
            1,
            "but the registry it loads already reads the queue as offered"
        );
        assert!(
            pending
                .offer_transfer(
                    &transfer("t1"),
                    &event(EventKind::DaemonTransferOffered {
                        transfer: "t1".into(),
                        successor_pid: 2
                    })
                )
                .unwrap()
        );
        assert!(
            pending
                .settle_transfer(
                    "t1",
                    None,
                    TransferState::Aborted,
                    now,
                    &event(EventKind::DaemonTransferAborted {
                        transfer: "t1".into(),
                        reason: "test".into()
                    })
                )
                .unwrap()
        );
        assert_eq!(stored(&pending), 0, "aborted: still as v18 wrote it");
        assert!(
            pending.conversation_heads().unwrap().is_empty(),
            "aborted archive is empty"
        );
        drop(pending);
        let pending = Store::open_pending(&path).unwrap();
        assert!(
            pending
                .offer_transfer(
                    &transfer("t2"),
                    &event(EventKind::DaemonTransferOffered {
                        transfer: "t2".into(),
                        successor_pid: 2
                    })
                )
                .unwrap()
        );
        pending.reject_event_for_test("daemon_transfer_accepted");
        assert!(
            pending
                .settle_transfer(
                    "t2",
                    Some(2),
                    TransferState::Accepted,
                    now,
                    &event(EventKind::DaemonTransferAccepted {
                        transfer: "t2".into()
                    }),
                )
                .is_err()
        );
        assert_eq!(pending.recorded_schema_version().unwrap(), 18);
        assert_eq!(
            stored(&pending),
            0,
            "failed acceptance rolls back migration"
        );
        assert!(pending.conversation_heads().unwrap().is_empty());
        assert_eq!(
            pending.transfer().unwrap().unwrap().state,
            TransferState::Offered
        );
        pending
            .conn
            .execute_batch("DROP TRIGGER reject_event")
            .unwrap();
        assert!(
            pending
                .settle_transfer(
                    "t2",
                    Some(2),
                    TransferState::Accepted,
                    now,
                    &event(EventKind::DaemonTransferAccepted {
                        transfer: "t2".into()
                    })
                )
                .unwrap()
        );
        assert_eq!(stored(&pending), 1, "accepted: the queued row is an offer");
        assert_eq!(offers(&pending), 1);
        assert_eq!(pending.recorded_schema_version().unwrap(), SCHEMA_VERSION);
        let found = pending.search_messages("old", None, None, 50).unwrap();
        assert_eq!(
            found.len(),
            1,
            "new archive is searchable immediately after acceptance"
        );
        let archived_id = found[0].envelope.id.clone();
        let archived_seq = found[0].seq;
        drop(pending);
        let reopened = Store::open(&path).unwrap();
        let found = reopened.search_messages("old", None, None, 50).unwrap();
        assert_eq!(
            found.len(),
            1,
            "index rebuild does not duplicate the backfill"
        );
        assert_eq!(found[0].envelope.id, archived_id);
        assert_eq!(found[0].seq, archived_seq);
        assert_eq!(offers(&reopened), 1, "and once only");
        assert_eq!(stored(&reopened), 1);
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
    fn legacy_schemas_upgrade_to_durable_delivery_guard() {
        for version in 1..SCHEMA_VERSION {
            let conn = crate::sqlite_fixture::in_memory().unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version', ?1)",
                [version.to_string()],
            )
            .unwrap();
            let store = Store::init(conn, true).unwrap();
            let version: String = store
                .conn
                .query_row(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION.to_string());
        }
    }

    fn archive_fixture() -> (Store, Vec<Envelope>) {
        use agentdocker_core::{AgentSpec, Destination};
        let conn = crate::sqlite_fixture::in_memory().unwrap();
        let store = Store::init(conn, true).unwrap();
        let a = AgentRecord::new(AgentSpec::default(), false, Utc::now());
        let b = AgentRecord::new(AgentSpec::default(), false, Utc::now());
        let mut sent = Vec::new();
        // Twelve in one direct conversation, spaced a minute apart and old,
        // then three in a channel, recent.
        for n in 0..12 {
            let at = Utc::now() - chrono::Duration::hours(2) + chrono::Duration::minutes(n);
            let envelope = Envelope::new(
                a.id.as_str(),
                Destination::Agent(b.id.clone()),
                "chat",
                serde_json::json!({ "text": format!("old {n} lantern") }),
                None,
                at,
            );
            store
                .publish_message(
                    &envelope,
                    std::slice::from_ref(&b.id),
                    1000,
                    None,
                    None,
                    None,
                    &[],
                )
                .unwrap();
            sent.push(envelope);
        }
        for n in 0..3 {
            let envelope = Envelope::new(
                b.id.as_str(),
                Destination::Channel(agentdocker_core::ChannelId::from("room".to_owned())),
                "chat",
                serde_json::json!({ "text": format!("new {n}") }),
                None,
                Utc::now(),
            );
            store
                .publish_message(
                    &envelope,
                    std::slice::from_ref(&a.id),
                    1000,
                    None,
                    None,
                    None,
                    &[],
                )
                .unwrap();
            sent.push(envelope);
        }
        (store, sent)
    }

    /// Pruning spends one budget for the whole tick across the age cutoff
    /// and every crowded conversation, and never takes a conversation's
    /// newest message, so its head and last line survive.
    #[test]
    fn pruning_keeps_heads_and_spends_one_budget_per_tick() {
        let (store, sent) = archive_fixture();
        let dm = ConversationId::of(&sent[0]).unwrap();
        let room = ConversationId::of(&sent[12]).unwrap();
        let cutoff = Some(Utc::now() - chrono::Duration::hours(1));
        // Age would take eleven old rows (the twelfth is the head); the cap
        // of one would take two of the room's three; the budget allows five.
        let removed = store.prune_messages(cutoff, 1, 5).unwrap();
        assert_eq!(
            removed, 5,
            "the budget bounds the tick, not each conversation"
        );
        let removed = store.prune_messages(cutoff, 1, 100).unwrap();
        assert_eq!(
            removed,
            6 + 2,
            "the rest of the old rows, then the room's excess"
        );
        let heads = store.conversation_heads().unwrap();
        let head_of = |c: &ConversationId| heads.iter().find(|m| m.conversation == *c).cloned();
        assert_eq!(
            head_of(&dm).map(|m| m.envelope.id),
            Some(sent[11].id.clone()),
            "the direct conversation kept its newest message"
        );
        assert_eq!(
            head_of(&room).map(|m| m.envelope.id),
            Some(sent[14].id.clone()),
            "the room kept its newest message"
        );
        assert_eq!(store.history(&dm, None, 100).unwrap().len(), 1);
        assert_eq!(store.history(&room, None, 100).unwrap().len(), 1);
        assert_eq!(
            store.prune_messages(cutoff, 1, 100).unwrap(),
            0,
            "nothing more to take"
        );
    }

    /// Search uses the index while it answers and falls back to LIKE the
    /// moment it fails, without a restart.
    #[test]
    fn search_falls_back_to_like_when_the_index_fails() {
        let (store, sent) = archive_fixture();
        let found = store.search_messages("lantern", None, None, 50).unwrap();
        assert_eq!(found.len(), 12);
        assert!(
            found.windows(2).all(|w| w[0].seq > w[1].seq),
            "newest first"
        );
        let scoped = store
            .search_messages(
                "lantern",
                Some(&[ConversationId::of(&sent[12]).unwrap()]),
                None,
                50,
            )
            .unwrap();
        assert!(
            scoped.is_empty(),
            "scoped to the room, where nobody said it"
        );
        assert!(
            store
                .search_messages("lantern", Some(&[]), None, 50)
                .unwrap()
                .is_empty()
        );
        if store.fts {
            store.conn.execute("DROP TABLE messages_fts", []).unwrap();
            let found = store.search_messages("lantern", None, None, 50).unwrap();
            assert_eq!(found.len(), 12, "answered by LIKE once the index failed");
            assert!(!store.messages_fts.get());
            // Later writes and searches keep working without the index.
            let extra = Envelope::new(
                "x",
                agentdocker_core::Destination::Broadcast,
                "chat",
                serde_json::json!({ "text": "lantern again" }),
                None,
                Utc::now(),
            );
            store
                .publish_message(&extra, &[], 1000, None, None, None, &[])
                .unwrap();
            assert_eq!(
                store
                    .search_messages("lantern", None, None, 50)
                    .unwrap()
                    .len(),
                13
            );
        }
    }

    /// A term the index cannot take literally is found by LIKE with the
    /// wildcards escaped, so `%`, `_`, a backslash or a quote match only
    /// themselves, indexed or not.
    #[test]
    fn punctuation_terms_search_literally_on_both_paths() {
        use agentdocker_core::Destination;
        let store = Store::in_memory().unwrap();
        let said = |text: &str| {
            Envelope::new(
                "x",
                Destination::Broadcast,
                "chat",
                serde_json::json!({ "text": text }),
                None,
                Utc::now(),
            )
        };
        for text in [
            "100% done",
            "snake_case",
            r"back\slash",
            "a \"quoted\" word",
            "plain words",
        ] {
            store
                .publish_message(&said(text), &[], 1000, None, None, None, &[])
                .unwrap();
        }
        for (term, expected) in [
            ("%", 1),
            ("_", 1),
            ("\\", 1),
            ("\"quoted\"", 1),
            ("%_", 0),
            ("words", 1),
        ] {
            let found = store.search_messages(term, None, None, 50).unwrap();
            assert_eq!(found.len(), expected, "term {term:?} found {}", found.len());
        }
        assert_eq!(
            store
                .search_messages("words", None, None, 50)
                .unwrap()
                .len(),
            store.without_fts_search("words").unwrap().len(),
            "the indexed and the literal path agree on a word"
        );
    }

    /// Once the index has failed, every later write skips it and the
    /// completeness marker is gone, so a restart with the table still there
    /// rebuilds it and finds what was archived meanwhile.
    #[test]
    fn a_failed_index_is_rebuilt_at_the_next_start() {
        use agentdocker_core::Destination;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        if !store.fts {
            return;
        }
        let said = |text: &str| {
            Envelope::new(
                "x",
                Destination::Broadcast,
                "chat",
                serde_json::json!({ "text": text }),
                None,
                Utc::now(),
            )
        };
        store
            .publish_message(&said("first lantern"), &[], 1000, None, None, None, &[])
            .unwrap();
        // A query fails while the table is out of reach. Search disables
        // the index in memory without writing during a coordinator fence;
        // the next unindexed archive mutation must persist that fact.
        store
            .conn
            .execute("ALTER TABLE messages_fts RENAME TO messages_fts_away", [])
            .unwrap();
        let changes_before_search = store.conn.total_changes();
        assert_eq!(
            store
                .search_messages("lantern", None, None, 50)
                .unwrap()
                .len(),
            1,
            "answered by LIKE when the query fails"
        );
        assert_eq!(
            store.conn.total_changes(),
            changes_before_search,
            "search is read-only"
        );
        assert!(!store.messages_fts.get(), "the index is off for this run");
        store
            .conn
            .execute("ALTER TABLE messages_fts_away RENAME TO messages_fts", [])
            .unwrap();
        let marker: Option<String> = store
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='messages_fts_complete'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(
            marker.as_deref(),
            Some("1"),
            "a read changes no durable state"
        );
        store
            .publish_message(&said("second lantern"), &[], 1000, None, None, None, &[])
            .unwrap();
        let marker: Option<String> = store
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='messages_fts_complete'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(marker, None, "unindexed writes clear the marker atomically");
        assert_eq!(
            store
                .search_messages("lantern", None, None, 50)
                .unwrap()
                .len(),
            2,
            "answered by LIKE while the index is off"
        );
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert!(reopened.messages_fts.get(), "rebuilt at start");
        let indexed: i64 = reopened
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'lantern'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            indexed, 2,
            "the message archived while the index was off is in it"
        );
        assert_eq!(
            reopened
                .search_messages("lantern", None, None, 50)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn a_rolled_back_index_failure_stays_incomplete_when_row_counts_balance() {
        use agentdocker_core::Destination;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        if !store.messages_fts.get() {
            return;
        }
        let said = |text: &str| {
            Envelope::new(
                "sender",
                Destination::Broadcast,
                "chat",
                serde_json::json!({"text": text}),
                None,
                Utc::now(),
            )
        };
        let old = said("oldsearchtoken");
        store
            .publish_message(&old, &[], 1000, None, None, None, &[])
            .unwrap();
        let old_seq = store.archived(&old.id).unwrap().unwrap().seq;
        // The index failure disables it in memory, while rollback restores
        // its table, archive rows and the old durable completeness marker.
        let tx = store.conn.unchecked_transaction().unwrap();
        store
            .conn
            .execute("ALTER TABLE messages_fts RENAME TO messages_fts_away", [])
            .unwrap();
        let failed = said("rolledbacktoken");
        store
            .archive_message(&store.conn, &failed, &ConversationId::of(&failed).unwrap())
            .unwrap();
        assert!(!store.messages_fts.get());
        tx.rollback().unwrap();
        let marker: String = store
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='messages_fts_complete'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker, "1");
        // A successful append and deletion have the same final row count
        // as before the failure. Counts alone cannot establish completeness.
        let new = said("newsearchtoken");
        store
            .publish_message(&new, &[], 1000, None, None, None, &[])
            .unwrap();
        let tx = store.conn.unchecked_transaction().unwrap();
        store
            .delete_archived(&[i64::try_from(old_seq).unwrap()])
            .unwrap();
        tx.commit().unwrap();
        let count = |table: &str| -> i64 {
            store
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert_eq!(count("messages"), count("messages_fts"));
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert!(reopened.messages_fts.get());
        let found = reopened
            .search_messages("newsearchtoken", None, None, 50)
            .unwrap();
        assert_eq!(
            found.len(),
            1,
            "the successful append must be indexed after restart"
        );
        assert_eq!(found[0].envelope.id, new.id);
        assert!(
            reopened
                .search_messages("oldsearchtoken", None, None, 50)
                .unwrap()
                .is_empty()
        );
    }

    /// A thread pages by the last reply's seq without repeats or gaps, and
    /// never brings in a reply from another conversation.
    #[test]
    fn a_thread_pages_by_seq_within_its_conversation() {
        use agentdocker_core::Destination;
        let conn = crate::sqlite_fixture::in_memory().unwrap();
        let store = Store::init(conn, true).unwrap();
        let root = Envelope::new(
            "a",
            Destination::Broadcast,
            "chat",
            serde_json::json!({ "text": "root" }),
            None,
            Utc::now(),
        );
        store
            .publish_message(&root, &[], 1000, None, None, None, &[])
            .unwrap();
        let mut replies = Vec::new();
        for n in 0..7 {
            let reply = Envelope::new(
                "b",
                Destination::Broadcast,
                "chat",
                serde_json::json!({ "text": format!("reply {n}") }),
                Some(root.id.clone()),
                Utc::now(),
            );
            store
                .publish_message(&reply, &[], 1000, None, None, None, &[])
                .unwrap();
            replies.push(reply.id.clone());
        }
        // A reply from another conversation naming the same root.
        let elsewhere = Envelope::new(
            "b",
            Destination::Channel(agentdocker_core::ChannelId::from("room".to_owned())),
            "chat",
            serde_json::json!({ "text": "not this thread" }),
            Some(root.id.clone()),
            Utc::now(),
        );
        store
            .publish_message(&elsewhere, &[], 1000, None, None, None, &[])
            .unwrap();
        let all = ConversationId::all();
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let page = store.thread_replies(&root.id, &all, after, 3).unwrap();
            if page.is_empty() {
                break;
            }
            assert!(page.len() <= 3);
            after = page.last().map(|m| m.seq);
            seen.extend(page.into_iter().map(|m| m.envelope.id));
        }
        assert_eq!(
            seen, replies,
            "every reply once, in order, none from elsewhere"
        );
        assert_eq!(
            store.archived(&root.id).unwrap().unwrap().replies,
            7,
            "the root counts only its own conversation's replies"
        );
    }

    /// A database from before the archive: what was queued is archived in
    /// its conversation, and the person's cursor starts at every head, so
    /// the past is not the first unread count; an agent's queue is as it
    /// was.
    #[test]
    fn the_backfilled_archive_starts_read_for_the_person() {
        use agentdocker_core::{AgentSpec, Destination};
        let conn = crate::sqlite_fixture::in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('schema_version', '20')",
            [],
        )
        .unwrap();
        let now = Utc::now();
        let mut human = AgentRecord::new(AgentSpec::default(), false, now);
        human.spec.name = "user".into();
        human.spec.runtime = agentdocker_core::HUMAN_RUNTIME.into();
        let agent = AgentRecord::new(AgentSpec::default(), false, now);
        for (name, record) in [("user", &human), ("agent", &agent)] {
            conn.execute(
                "INSERT INTO agents (id, name, live, created_at, json) VALUES (?1, ?2, 1, ?3, ?4)",
                params![
                    record.id.as_str(),
                    name,
                    now.to_rfc3339(),
                    serde_json::to_string(record).unwrap()
                ],
            )
            .unwrap();
        }
        let queued = [
            Envelope::new(
                agent.id.as_str(),
                Destination::Agent(human.id.clone()),
                "chat",
                serde_json::json!({ "text": "to the person" }),
                None,
                now,
            ),
            Envelope::new(
                human.id.as_str(),
                Destination::Agent(agent.id.clone()),
                "chat",
                serde_json::json!({ "text": "to the agent" }),
                None,
                now,
            ),
        ];
        for (to, envelope) in [(&human, &queued[0]), (&agent, &queued[1])] {
            conn.execute(
                "INSERT INTO inbox (agent, message_id, json) VALUES (?1, ?2, ?3)",
                params![
                    to.id.as_str(),
                    envelope.id.as_str(),
                    serde_json::to_string(envelope).unwrap()
                ],
            )
            .unwrap();
        }
        let store = Store::init(conn, true).unwrap();
        let dm = ConversationId::dm(human.id.as_str(), agent.id.as_str());
        let archived = store.history(&dm, None, 10).unwrap();
        assert_eq!(archived.len(), 2, "both queued rows are in the archive");
        let cursor = store
            .read_cursors(human.id.as_str())
            .unwrap()
            .into_iter()
            .find(|c| c.conversation == dm)
            .expect("the person's cursor is at the head");
        assert_eq!(cursor.through, archived[1].seq);
        assert_eq!(
            store
                .unread_after(&dm, cursor.through, std::slice::from_ref(&human.id))
                .unwrap(),
            0
        );
        assert!(
            store.read_cursors(agent.id.as_str()).unwrap().is_empty(),
            "an agent's queue is as it was"
        );
    }

    /// A database written before v19 has queued messages nobody recorded
    /// an offer for. Opened by this build, every one of them is an offer,
    /// so a binding made afterwards carries them as uncertain; messages
    /// queued after the upgrade are recorded as they are offered.
    #[test]
    fn queued_messages_from_before_v19_are_offered_at_the_upgrade() {
        use agentdocker_core::{AgentSpec, Destination};
        let conn = crate::sqlite_fixture::in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('schema_version', '18')",
            [],
        )
        .unwrap();
        let now = Utc::now();
        let mut record = AgentRecord::new(AgentSpec::default(), false, now);
        record.spec.name = "old".into();
        let mut json = serde_json::to_value(&record).unwrap();
        // The row as an old daemon wrote it: no bookkeeping fields at all.
        json.as_object_mut().unwrap().remove("legacy_offers");
        json.as_object_mut().unwrap().remove("input_binding");
        conn.execute(
            "INSERT INTO agents (id, name, live, created_at, json) VALUES (?1, 'old', 1, ?2, ?3)",
            params![record.id.as_str(), now.to_rfc3339(), json.to_string()],
        )
        .unwrap();
        let queued: Vec<Envelope> = (0..2)
            .map(|n| {
                Envelope::new(
                    "peer",
                    Destination::Agent(record.id.clone()),
                    "chat",
                    serde_json::json!({ "text": format!("old {n}") }),
                    None,
                    now,
                )
            })
            .collect();
        for envelope in &queued {
            conn.execute(
                "INSERT INTO inbox (agent, message_id, json) VALUES (?1, ?2, ?3)",
                params![
                    record.id.as_str(),
                    envelope.id.as_str(),
                    serde_json::to_string(envelope).unwrap()
                ],
            )
            .unwrap();
        }
        let untouched = AgentRecord::new(AgentSpec::default(), false, now);
        conn.execute(
            "INSERT INTO agents (id, name, live, created_at, json) VALUES (?1, 'empty', 1, ?2, ?3)",
            params![
                untouched.id.as_str(),
                now.to_rfc3339(),
                serde_json::to_string(&untouched).unwrap()
            ],
        )
        .unwrap();
        let store = Store::init(conn, true).unwrap();
        let agents = store.load_agents().unwrap();
        let old = agents.iter().find(|a| a.id == record.id).unwrap();
        assert_eq!(old.legacy_offers.len(), 2);
        for envelope in &queued {
            assert!(old.legacy_offers[&envelope.id] >= now);
        }
        let empty = agents.iter().find(|a| a.id == untouched.id).unwrap();
        assert!(empty.legacy_offers.is_empty());
        let version: String = store
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    /// A v19 daemon could hand a synchronous ask its answer and leave the
    /// row queued with nothing recorded. Opened by this build, every
    /// queued correlated reply is an offer, on the record and in a
    /// standing binding's uncertain set; ordinary messages are not.
    #[test]
    fn correlated_replies_from_before_v20_are_offered_at_the_upgrade() {
        use agentdocker_core::{
            AgentSpec, Destination, InputBinding, ProcessIdentity, ProviderGeneration,
        };
        let conn = crate::sqlite_fixture::in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('schema_version', '19')",
            [],
        )
        .unwrap();
        let now = Utc::now();
        let mut record = AgentRecord::new(AgentSpec::default(), false, now);
        let process = ProcessIdentity {
            pid: 4242,
            started_at: now,
        };
        record.input_binding = Some(InputBinding {
            provider: ProviderGeneration {
                process: process.clone(),
                session: "thread".into(),
                profile: "/profile".into(),
            },
            controller: process,
            controller_since: now,
            token_sha256: "digest".into(),
            bound_at: now,
            controller_generations: 1,
            uncertain: Vec::new(),
            launch: None,
            restart: Default::default(),
        });
        conn.execute(
            "INSERT INTO agents (id, name, live, created_at, json) VALUES (?1, 'bound', 1, ?2, ?3)",
            params![
                record.id.as_str(),
                now.to_rfc3339(),
                serde_json::to_string(&record).unwrap()
            ],
        )
        .unwrap();
        let message = |reply_to: Option<MessageId>| {
            Envelope::new(
                "peer",
                Destination::Agent(record.id.clone()),
                "answer",
                serde_json::json!({ "text": "x" }),
                reply_to,
                now,
            )
        };
        let plain = message(None);
        let reply = message(Some(MessageId::generate()));
        for envelope in [&plain, &reply] {
            conn.execute(
                "INSERT INTO inbox (agent, message_id, json) VALUES (?1, ?2, ?3)",
                params![
                    record.id.as_str(),
                    envelope.id.as_str(),
                    serde_json::to_string(envelope).unwrap()
                ],
            )
            .unwrap();
        }
        let store = Store::init(conn, true).unwrap();
        let bound = store
            .load_agents()
            .unwrap()
            .into_iter()
            .find(|a| a.id == record.id)
            .unwrap();
        assert!(bound.legacy_offers.contains_key(&reply.id));
        assert!(!bound.legacy_offers.contains_key(&plain.id));
        assert_eq!(bound.input_binding.unwrap().uncertain, vec![reply.id]);
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
            amount: 0,
            automatic: false,
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
        store
            .delete_agent(
                &AgentId::from("a1"),
                &Event::new(EventKind::AgentRemoved { agent: "a1".into() }, Utc::now()),
            )
            .unwrap();
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
        let legacy = crate::sqlite_fixture::in_memory().unwrap();
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
        let store = Store::init(legacy, true).unwrap().without_fts();
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
        let legacy = crate::sqlite_fixture::open(&path).unwrap();
        legacy.execute_batch("CREATE TABLE journal (
            id INTEGER PRIMARY KEY, project TEXT NOT NULL, seq INTEGER NOT NULL,
            at TEXT NOT NULL, agent TEXT, branch TEXT, kind TEXT NOT NULL,
            json TEXT NOT NULL, UNIQUE(project, seq));
            INSERT INTO journal (project, seq, at, kind, json) VALUES ('p', 7, '', 'note', 'malformed');").unwrap();
        drop(legacy);
        assert!(Store::open(&path).is_err());
        let conn = crate::sqlite_fixture::open(&path).unwrap();
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

    fn aged_entry(project: &str, seq: u64, age: Duration) -> JournalEntry {
        serde_json::from_value(serde_json::json!({
            "project":project, "seq":seq, "at":Utc::now() - age, "agent_name":"writer",
            "kind":"note", "summary":format!("entry {seq}"), "summary_source":"explicit"
        }))
        .unwrap()
    }

    #[test]
    fn retention_boundary_is_batched_and_skips_fresh_projects() {
        let store = Store::in_memory().unwrap();
        let old = ProjectId::from("old");
        let fresh = ProjectId::from("fresh");
        // Ten expired entries, then two inside the window.
        for seq in 1..=10 {
            store
                .append_journal(&aged_entry("old", seq, Duration::days(20 - seq as i64)))
                .unwrap();
        }
        store
            .append_journal(&aged_entry("old", 11, Duration::hours(1)))
            .unwrap();
        store
            .append_journal(&aged_entry("old", 12, Duration::minutes(1)))
            .unwrap();
        store
            .append_journal(&aged_entry("fresh", 1, Duration::hours(2)))
            .unwrap();
        let cutoff = Utc::now() - Duration::days(7);
        assert_eq!(
            store.journal_projects().unwrap(),
            vec![fresh.clone(), old.clone()]
        );
        assert_eq!(
            store
                .journal_retention_boundary(&fresh, cutoff, 1_000)
                .unwrap(),
            None
        );
        // A batch of four takes the four oldest; the rest wait for the next tick.
        assert_eq!(
            store.journal_retention_boundary(&old, cutoff, 4).unwrap(),
            Some(5)
        );
        assert_eq!(store.prune_journal(&old, 5).unwrap(), 4);
        // An unbounded batch stops at the window, never at a fresh entry.
        assert_eq!(
            store
                .journal_retention_boundary(&old, cutoff, 1_000)
                .unwrap(),
            Some(11)
        );
        assert_eq!(store.prune_journal(&old, 11).unwrap(), 6);
        assert_eq!(
            store
                .journal_retention_boundary(&old, cutoff, 1_000)
                .unwrap(),
            None
        );
        // A clock that went backwards: an expired entry behind a fresh one
        // does not drag the fresh one out. Retention stops at the fresh row.
        let skew = ProjectId::from("skew");
        store
            .append_journal(&aged_entry("skew", 1, Duration::days(30)))
            .unwrap();
        store
            .append_journal(&aged_entry("skew", 2, Duration::hours(1)))
            .unwrap();
        store
            .append_journal(&aged_entry("skew", 3, Duration::days(30)))
            .unwrap();
        assert_eq!(
            store
                .journal_retention_boundary(&skew, cutoff, 1_000)
                .unwrap(),
            Some(2)
        );
        let mut query = JournalQuery::new(old.clone(), 50);
        query.since_seq = None;
        let left: Vec<u64> = store
            .journal(&query)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(left, [11, 12]);
        // The head survives pruning, so seqs keep counting up after a restart.
        assert_eq!(store.max_journal_seq(&old).unwrap(), 12);
        let (before, after) = store.vacuum().unwrap();
        assert!(
            before > 0 && after > 0 && after <= before,
            "{before} -> {after}"
        );
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
