//! On-demand accounting storage inspection, outside the running daemon.
//!
//! Logical admission bytes, SQLite btree allocation and filesystem lengths are
//! different measurements. Never label their difference as injected tokens.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_PAGES: u64 = 131_072;
const MAX_OBJECTS: usize = 128;
const DEADLINE: Duration = Duration::from_secs(10);

/// File lengths are sampled outside the SQL snapshot. WAL may change between
/// observations and contains transactions for more than accounting tables.
#[derive(Debug, Serialize)]
pub struct FileLengths {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub shm_bytes: u64,
}

/// A complete bounded snapshot of accounting btrees, including their indexes.
/// Unused bytes are already included in table/index page bytes, not added to
/// them. Database page bytes also include unrelated state and free pages.
#[derive(Debug, Serialize)]
pub struct Report {
    pub measured_at: chrono::DateTime<chrono::Utc>,
    pub elapsed_ms: u128,
    pub state_schema: u32,
    pub logical_tracking_bytes: u64,
    pub logical_tracking_capacity_bytes: u64,
    pub accounting_table_count: u64,
    pub accounting_index_count: u64,
    pub accounting_table_page_bytes: u64,
    pub accounting_index_page_bytes: u64,
    pub accounting_payload_bytes: u64,
    pub accounting_unused_page_bytes: u64,
    pub database_page_bytes: u64,
    pub database_free_page_bytes: u64,
    pub files_before: FileLengths,
    pub files_after: FileLengths,
}

fn unsigned(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    value
        .try_into()
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn companion(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn file_length(path: &Path, required: bool) -> Result<u64> {
    match agentdocker_host::dirs::read_private_file(path) {
        Ok(file) => Ok(file.metadata()?.len()),
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

fn file_lengths(path: &Path) -> Result<FileLengths> {
    Ok(FileLengths {
        database_bytes: file_length(path, true)?,
        wal_bytes: file_length(&companion(path, "-wal"), false)?,
        shm_bytes: file_length(&companion(path, "-shm"), false)?,
    })
}

/// Inspect an explicitly selected existing private state home. This opens a
/// separate read-only SQLite connection: no Store initialization, migration,
/// checkpoint, vacuum, daemon connection or background collector is performed.
/// Page/time limits are checked between SQLite page steps; OS I/O itself has
/// no hard real-time guarantee. An exceeded bound returns no partial report.
pub fn inspect(home: &Path) -> Result<Report> {
    inspect_bounded(home, MAX_PAGES, DEADLINE)
}

fn inspect_bounded(home: &Path, max_pages: u64, deadline: Duration) -> Result<Report> {
    crate::initialize_storage_platform()?;
    let started = Instant::now();
    let home = home
        .canonicalize()
        .context("state home must already exist")?;
    agentdocker_host::dirs::check_private_dir(&home)?;
    let path = home.join("state.db");
    let original = agentdocker_host::dirs::read_private_file(&path)?;
    let original = same_file::Handle::from_file(original)?;
    let files_before = file_lengths(&path)?;
    // SQLite may inspect a rollback journal even though this connection never
    // writes. Existing companions must meet the same private-file policy.
    file_length(&companion(&path, "-journal"), false)?;
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_millis(250))?;
    conn.pragma_update(None, "query_only", true)?;
    let tx = conn.unchecked_transaction()?;
    let schema: String = tx.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    let state_schema: u32 = schema.parse().context("stored state schema is invalid")?;
    ensure!(
        (23..=crate::STATE_SCHEMA_VERSION).contains(&state_schema),
        "storage inspection requires a supported accounting state schema"
    );
    let (logical_tracking_bytes, logical_tracking_capacity_bytes): (u64, u64) = tx
        .query_row(
            "SELECT bytes,capacity FROM usage_tracking WHERE singleton=1",
            [],
            |row| Ok((unsigned(row, 0)?, unsigned(row, 1)?)),
        )
        .context("state has no initialized accounting tracking budget")?;
    let page_size: u64 = tx.pragma_query_value(None, "page_size", |row| unsigned(row, 0))?;
    let pages: u64 = tx.pragma_query_value(None, "page_count", |row| unsigned(row, 0))?;
    let free: u64 = tx.pragma_query_value(None, "freelist_count", |row| unsigned(row, 0))?;
    let database_page_bytes = pages
        .checked_mul(page_size)
        .context("database size overflow")?;
    let database_free_page_bytes = free
        .checked_mul(page_size)
        .context("free-page size overflow")?;
    ensure!(free <= pages, "invalid database free-page count");
    let mut report = Report {
        measured_at: chrono::Utc::now(),
        elapsed_ms: 0,
        state_schema,
        logical_tracking_bytes,
        logical_tracking_capacity_bytes,
        accounting_table_count: 0,
        accounting_index_count: 0,
        accounting_table_page_bytes: 0,
        accounting_index_page_bytes: 0,
        accounting_payload_bytes: 0,
        accounting_unused_page_bytes: 0,
        database_page_bytes,
        database_free_page_bytes,
        files_before,
        files_after: FileLengths {
            database_bytes: 0,
            wal_bytes: 0,
            shm_bytes: 0,
        },
    };
    let objects = {
        let mut statement = tx.prepare(
            "SELECT name,type FROM sqlite_schema
             WHERE substr(tbl_name,1,6)='usage_' AND type IN ('table','index') AND rootpage>0
             LIMIT 129",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    ensure!(
        objects.len() <= MAX_OBJECTS,
        "accounting storage object limit exceeded"
    );
    ensure!(
        objects
            .iter()
            .any(|(name, kind)| name == "usage_tracking" && kind == "table"),
        "tracking budget is not an accounting table"
    );
    let mut scanned = 0u64;
    for (name, kind) in objects {
        ensure!(
            started.elapsed() < deadline,
            "accounting storage inspection timed out"
        );
        // A name equality constrains DBSTAT to this btree. Page mode allows
        // bounds between rows rather than aggregating a whole btree inside a
        // single SQLite step. sqlite_schema associates automatic indexes too.
        let mut statement = tx.prepare(
            "SELECT pgsize,payload,unused FROM dbstat('main') WHERE name=?1 AND aggregate=FALSE",
        )?;
        let mut rows = statement.query([name])?;
        let mut object_pages = 0u64;
        while let Some(row) = rows.next()? {
            scanned += 1;
            object_pages += 1;
            ensure!(
                scanned <= max_pages,
                "accounting storage page limit exceeded"
            );
            ensure!(
                started.elapsed() < deadline,
                "accounting storage inspection timed out"
            );
            let bytes = unsigned(row, 0)?;
            let payload = unsigned(row, 1)?;
            let unused = unsigned(row, 2)?;
            ensure!(
                bytes == page_size && payload <= bytes && unused <= bytes - payload,
                "invalid accounting page measurement"
            );
            let total = if kind == "table" {
                &mut report.accounting_table_page_bytes
            } else {
                &mut report.accounting_index_page_bytes
            };
            *total = total
                .checked_add(bytes)
                .context("accounting page size overflow")?;
            report.accounting_payload_bytes = report
                .accounting_payload_bytes
                .checked_add(payload)
                .context("accounting payload size overflow")?;
            report.accounting_unused_page_bytes = report
                .accounting_unused_page_bytes
                .checked_add(unused)
                .context("accounting unused size overflow")?;
        }
        ensure!(object_pages > 0, "accounting btree has no measured pages");
        if kind == "table" {
            report.accounting_table_count += 1;
        } else {
            report.accounting_index_count += 1;
        }
    }
    ensure!(
        report
            .accounting_table_page_bytes
            .checked_add(report.accounting_index_page_bytes)
            .is_some_and(|used| used <= database_page_bytes - database_free_page_bytes),
        "accounting pages exceed allocated database pages"
    );
    tx.rollback()?;
    report.files_after = file_lengths(&path)?;
    ensure!(
        same_file::Handle::from_file(agentdocker_host::dirs::read_private_file(&path)?)?
            == original,
        "state database was replaced during inspection"
    );
    report.elapsed_ms = started.elapsed().as_millis();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_live_wal_tables_indexes_and_overflow_without_changing_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = crate::store::Store::open(&path).unwrap();
        let conn = crate::sqlite_fixture::open(&path).unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch(
            "CREATE TABLE usage_fixture(key TEXT PRIMARY KEY, payload BLOB);
            CREATE INDEX usage_fixture_payload ON usage_fixture(payload);
            CREATE TABLE unrelated_fixture(payload BLOB);
            INSERT INTO usage_fixture VALUES ('one',zeroblob(100000));
            INSERT INTO unrelated_fixture VALUES (zeroblob(200000));",
        )
        .unwrap();
        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_fixture", [], |r| r.get(0))
            .unwrap();
        let report = inspect(dir.path()).unwrap();
        assert!(report.accounting_table_page_bytes >= 100000);
        assert!(report.accounting_index_page_bytes >= 100000);
        assert!(report.accounting_payload_bytes >= 200000);
        assert!(
            report.database_page_bytes
                > report.accounting_table_page_bytes + report.accounting_index_page_bytes + 200000
        );
        assert!(report.files_before.wal_bytes > 0);
        assert_eq!(report.logical_tracking_bytes, 0);
        assert_eq!(report.logical_tracking_capacity_bytes, 256 * 1024 * 1024);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM usage_fixture", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            before
        );
        let schema: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(schema, crate::STATE_SCHEMA_VERSION.to_string());
        let wire = serde_json::to_string(&report).unwrap();
        assert!(!wire.contains(&dir.path().display().to_string()));
        assert!(!wire.contains("usage_fixture"));
        drop(store);
    }

    #[test]
    fn bounded_inspection_never_returns_partial_totals() {
        let dir = tempfile::tempdir().unwrap();
        let _store = crate::store::Store::open(&dir.path().join("state.db")).unwrap();
        assert!(
            inspect_bounded(dir.path(), 0, DEADLINE)
                .unwrap_err()
                .to_string()
                .contains("page limit")
        );
        assert!(
            inspect_bounded(dir.path(), MAX_PAGES, Duration::ZERO)
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
    }

    #[test]
    fn absent_or_future_state_is_not_created_or_migrated() {
        let dir = tempfile::tempdir().unwrap();
        assert!(inspect(dir.path()).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let path = dir.path().join("state.db");
        let _store = crate::store::Store::open(&path).unwrap();
        let conn = crate::sqlite_fixture::open(&path).unwrap();
        conn.execute("UPDATE meta SET value='999' WHERE key='schema_version'", [])
            .unwrap();
        assert!(
            inspect(dir.path())
                .unwrap_err()
                .to_string()
                .contains("supported accounting")
        );
        assert_eq!(
            conn.query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "999"
        );
    }
}
