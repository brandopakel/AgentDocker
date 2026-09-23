//! Bound accounting admission without forgetting accepted dedupe evidence.
use super::*;

const MAX_BYTES: i64 = 256 * 1024 * 1024;
const GAP_KEY: &str = "tracking-capacity";
const TABLES: &[(&str, &[&str])] = &[
    (
        "usage_samples",
        &["source_id", "fingerprint", "at", "contribution"],
    ),
    ("usage_baselines", &["key", "json"]),
    ("usage_files", &["key", "json"]),
    (
        "usage_buckets",
        &["key", "hour", "agent", "project", "json"],
    ),
    (
        "usage_gaps",
        &["key", "since", "until", "runtime", "session", "reason"],
    ),
];

fn size(table: &str, columns: &[&str], prefix: &str) -> String {
    let bytes = columns
        .iter()
        .map(|column| format!("COALESCE(length(CAST({prefix}{column} AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    // Reserve one constant-size coverage marker even when accounting is full.
    // Everything else pays a per-row allowance in addition to its UTF-8 bytes.
    if table == "usage_gaps" {
        format!("CASE WHEN {prefix}key='{GAP_KEY}' THEN 0 ELSE 128+{bytes} END")
    } else {
        format!("128+{bytes}")
    }
}

/// Additive metadata: old writers also keep the byte total current through
/// these triggers. The accepted accounting rows and gap meanings do not change.
/// Backfill and trigger installation commit together, including pending opens.
pub(crate) fn init(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_tracking (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1),
        bytes INTEGER NOT NULL CHECK(bytes>=0),
        capacity INTEGER NOT NULL CHECK(capacity>=0 AND capacity<=268435456)
    )",
    )?;
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM usage_tracking)", [], |r| {
        r.get(0)
    })?;
    if !exists {
        let mut bytes = 0i64;
        for (table, columns) in TABLES {
            let amount: i64 = conn.query_row(
                &format!(
                    "SELECT COALESCE(SUM({}),0) FROM {table}",
                    size(table, columns, "")
                ),
                [],
                |r| r.get(0),
            )?;
            bytes = bytes
                .checked_add(amount)
                .context("usage tracking byte overflow")?;
        }
        conn.execute(
            "INSERT INTO usage_tracking VALUES (1,?1,?2)",
            params![bytes, MAX_BYTES],
        )?;
    }
    for (table, columns) in TABLES {
        let new = size(table, columns, "NEW.");
        let old = size(table, columns, "OLD.");
        for (operation, delta) in [
            ("INSERT", format!("+({new})")),
            ("UPDATE", format!("+({new})-({old})")),
            ("DELETE", format!("-({old})")),
        ] {
            conn.execute_batch(&format!(
                "CREATE TRIGGER IF NOT EXISTS {table}_tracking_{operation} AFTER {operation} ON {table}
                 BEGIN UPDATE usage_tracking SET bytes=bytes{delta} WHERE singleton=1; END;"
            ))?;
        }
    }
    tx.commit()?;
    Ok(())
}

impl Store {
    pub(super) fn usage_tracking_report(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Tracking> {
        let (bytes, capacity) = self.usage_tracking_size()?;
        let capacity_gap = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM usage_gaps WHERE key=?1 AND until>=?2 AND (since IS NULL OR since<?3))",
            params![GAP_KEY, text(since), text(until)], |r| r.get(0),
        )?;
        Ok(Tracking {
            logical_bytes: bytes.try_into()?,
            capacity_bytes: capacity.try_into()?,
            capacity_gap,
        })
    }

    fn usage_tracking_size(&self) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT bytes,capacity FROM usage_tracking WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Roll back only this accounting operation at capacity. SQLite failures
    /// still propagate normally; they are never disguised as a coverage gap.
    /// Nested calls use SQLite's innermost same-name savepoint semantics.
    pub(super) fn usage_bounded_write<T>(
        &self,
        write: impl FnOnce() -> Result<T>,
    ) -> Result<Option<T>> {
        let (before, capacity) = self.usage_tracking_size()?;
        self.conn.execute_batch("SAVEPOINT usage_tracking_write")?;
        let result = (|| {
            let value = write()?;
            let (after, _) = self.usage_tracking_size()?;
            Ok((value, after <= capacity || after <= before))
        })();
        match result {
            Ok((value, true)) => {
                self.conn.execute_batch("RELEASE usage_tracking_write")?;
                Ok(Some(value))
            }
            rejected => {
                self.conn.execute_batch(
                    "ROLLBACK TO usage_tracking_write; RELEASE usage_tracking_write",
                )?;
                match rejected {
                    Ok(_) => Ok(None),
                    Err(error) => Err(error),
                }
            }
        }
    }

    pub(super) fn usage_tracking_gap(&self, until: DateTime<Utc>) -> Result<u64> {
        let created: bool = self.conn.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM usage_gaps WHERE key=?1)",
            [GAP_KEY],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO usage_gaps VALUES (?1,NULL,?2,NULL,NULL,?3)
             ON CONFLICT(key) DO UPDATE SET until=MAX(until,excluded.until)",
            params![
                GAP_KEY,
                text(until),
                "usage tracking capacity reached; untracked records were not counted"
            ],
        )?;
        Ok(u64::from(created))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recompute(conn: &Connection) -> i64 {
        TABLES
            .iter()
            .map(|(table, columns)| {
                conn.query_row(
                    &format!(
                        "SELECT COALESCE(SUM({}),0) FROM {table}",
                        size(table, columns, "")
                    ),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
            })
            .sum()
    }

    #[test]
    fn tracking_bootstraps_old_rows_and_matches_updates_deletes_and_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.db");
        let store = Store::open(&db).unwrap();
        for (table, _) in TABLES {
            for operation in ["INSERT", "UPDATE", "DELETE"] {
                store
                    .conn
                    .execute_batch(&format!("DROP TRIGGER {table}_tracking_{operation}"))
                    .unwrap();
            }
        }
        store
            .conn
            .execute_batch(
                "DROP TABLE usage_tracking;
            INSERT INTO usage_samples VALUES ('old', 'hash', 'time', '日本語');
            INSERT INTO usage_baselines VALUES ('baseline','{}');
            INSERT INTO usage_files VALUES ('file','{}');
            INSERT INTO usage_buckets VALUES ('bucket','hour',NULL,NULL,'{}');
            INSERT INTO usage_gaps VALUES ('gap',NULL,'until',NULL,NULL,'reason');",
            )
            .unwrap();
        init(&store.conn).unwrap();
        let before = recompute(&store.conn);
        assert_eq!(store.usage_tracking_size().unwrap().0, before);
        let result = store.usage_bounded_write(|| -> Result<()> {
            store.conn.execute_batch(
                "UPDATE usage_samples SET contribution=NULL; DELETE FROM usage_baselines;",
            )?;
            anyhow::bail!("injected failure");
        });
        assert!(result.unwrap_err().to_string().contains("injected failure"));
        assert_eq!(store.usage_tracking_size().unwrap().0, before);
        assert_eq!(recompute(&store.conn), before);
        store
            .conn
            .execute_batch(
                "UPDATE usage_samples SET contribution=NULL; DELETE FROM usage_baselines;",
            )
            .unwrap();
        let after = recompute(&store.conn);
        assert!(after < before);
        assert_eq!(store.usage_tracking_size().unwrap().0, after);
        drop(store);
        let store = Store::open(&db).unwrap();
        assert_eq!(store.usage_tracking_size().unwrap().0, after);
        assert_eq!(recompute(&store.conn), after);
        assert!(store.conn.is_autocommit());
    }

    #[test]
    fn tracking_preserves_oversized_existing_state_but_allows_shrinking_it() {
        let store = Store::in_memory().unwrap();
        store.conn.execute_batch("INSERT INTO usage_files VALUES ('existing','longer'); UPDATE usage_tracking SET capacity=0;").unwrap();
        let before = store.usage_tracking_size().unwrap().0;
        assert!(
            store
                .usage_bounded_write(|| {
                    store
                        .conn
                        .execute("INSERT INTO usage_files VALUES ('new','{}')", [])?;
                    Ok(())
                })
                .unwrap()
                .is_none()
        );
        assert_eq!(store.usage_tracking_size().unwrap().0, before);
        assert!(
            store
                .usage_bounded_write(|| {
                    store
                        .conn
                        .execute("UPDATE usage_files SET json='{}' WHERE key='existing'", [])?;
                    Ok(())
                })
                .unwrap()
                .is_some()
        );
        assert!(store.usage_tracking_size().unwrap().0 < before);
        assert_eq!(
            store.usage_tracking_size().unwrap().0,
            recompute(&store.conn)
        );
        store.usage_tracking_gap(Utc::now()).unwrap();
        assert_eq!(
            store.usage_tracking_size().unwrap().0,
            recompute(&store.conn)
        );
    }
}
