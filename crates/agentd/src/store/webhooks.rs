//! The webhook delivery log: one row per attempt, bounded per sink and in
//! all, the oldest dropped as each new one lands.
use super::*;
use agentdocker_core::WebhookAttempt;

impl Store {
    /// Keep one attempt, then drop what is past `per_webhook` for its sink
    /// and past `total` across every sink, oldest first, in one write.
    pub(crate) fn log_webhook_attempt(
        &self,
        attempt: &WebhookAttempt,
        per_webhook: usize,
        total: usize,
    ) -> Result<()> {
        let tx = write_transaction(&self.conn)?;
        self.conn.execute(
            "INSERT INTO webhook_deliveries (webhook, json) VALUES (?1, ?2)",
            params![attempt.webhook, serde_json::to_string(attempt)?],
        )?;
        // The row at offset N, newest first, is the first one past the
        // bound: it and everything older go.
        self.conn.execute(
            "DELETE FROM webhook_deliveries WHERE webhook = ?1 AND seq <= (
                SELECT seq FROM webhook_deliveries WHERE webhook = ?1
                ORDER BY seq DESC LIMIT 1 OFFSET ?2)",
            params![attempt.webhook, i64::try_from(per_webhook)?],
        )?;
        self.conn.execute(
            "DELETE FROM webhook_deliveries WHERE seq <= (
                SELECT seq FROM webhook_deliveries ORDER BY seq DESC LIMIT 1 OFFSET ?1)",
            params![i64::try_from(total)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A sink's newest `limit` attempts, oldest first.
    pub(crate) fn webhook_attempts(
        &self,
        webhook: &str,
        limit: usize,
    ) -> Result<Vec<WebhookAttempt>> {
        let mut statement = self.conn.prepare(
            "SELECT json FROM webhook_deliveries WHERE webhook = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![webhook, i64::try_from(limit)?], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut attempts = rows
            .iter()
            .map(|json| serde_json::from_str(json).context("a webhook delivery row"))
            .collect::<Result<Vec<WebhookAttempt>>>()?;
        attempts.reverse();
        Ok(attempts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::AttemptResult;

    fn attempt(webhook: &str, seq: u64) -> WebhookAttempt {
        WebhookAttempt {
            webhook: webhook.into(),
            delivery: format!("{seq:032x}"),
            seq,
            event: "policy_updated".into(),
            attempt: 1,
            at: DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
            duration_ms: 3,
            result: AttemptResult::Delivered,
            status: Some(200),
            error: None,
            next_retry: None,
        }
    }

    fn count(store: &Store) -> i64 {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM webhook_deliveries", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    /// Each sink keeps its newest attempts up to its bound and the log
    /// keeps its newest up to the total, oldest dropped first; a read
    /// answers one sink's newest, oldest first, and survives a reopen.
    #[test]
    fn the_log_is_bounded_per_sink_and_in_all_and_kept_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = Store::open(&path).unwrap();
        for seq in 1..=7 {
            store
                .log_webhook_attempt(&attempt("alpha", seq), 5, 8)
                .unwrap();
        }
        let alpha = store.webhook_attempts("alpha", 100).unwrap();
        assert_eq!(
            alpha.iter().map(|a| a.seq).collect::<Vec<_>>(),
            vec![3, 4, 5, 6, 7],
            "the newest five, oldest first"
        );
        assert_eq!(
            store
                .webhook_attempts("alpha", 2)
                .unwrap()
                .iter()
                .map(|a| a.seq)
                .collect::<Vec<_>>(),
            vec![6, 7]
        );
        // Another sink's attempts push the oldest out of the whole log,
        // whichever sink they were for.
        for seq in 100..104 {
            store
                .log_webhook_attempt(&attempt("beta", seq), 5, 8)
                .unwrap();
        }
        assert_eq!(count(&store), 8);
        assert_eq!(
            store
                .webhook_attempts("alpha", 100)
                .unwrap()
                .iter()
                .map(|a| a.seq)
                .collect::<Vec<_>>(),
            vec![4, 5, 6, 7]
        );
        assert_eq!(store.webhook_attempts("beta", 100).unwrap().len(), 4);
        assert!(store.webhook_attempts("gamma", 100).unwrap().is_empty());
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.webhook_attempts("alpha", 100).unwrap(),
            (4..=7).map(|seq| attempt("alpha", seq)).collect::<Vec<_>>()
        );
    }
}
