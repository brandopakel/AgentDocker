//! Checked replay never turns a storage failure or retention gap into readiness.
use super::*;
use agentdocker_core::{
    EventCursor, Response,
    event::{EVENT_REPLAY_LIMIT, EVENT_STREAM_FRAME_BYTES},
};
use sha2::{Digest, Sha256};

pub(crate) struct EventReplay {
    pub start: EventCursor,
    pub head: EventCursor,
    pub frames: Vec<Response>,
}

fn cursor(log: &str, seq: u64, json: &str) -> EventCursor {
    EventCursor {
        log: log.to_owned(),
        seq,
        digest: if seq == 0 {
            String::new()
        } else {
            format!("{:x}", Sha256::digest(json.as_bytes()))
        },
    }
}

pub(crate) fn live_frame(log: &str, event: Event) -> Result<Response> {
    anyhow::ensure!(event.seq != 0, "live event has no durable sequence");
    let json = serde_json::to_string(&event)?;
    let frame = Response::EventAt {
        cursor: cursor(log, event.seq, &json),
        event,
    };
    anyhow::ensure!(
        serde_json::to_vec(&frame)?.len() < EVENT_STREAM_FRAME_BYTES,
        "event exceeds checked-stream frame bound"
    );
    Ok(frame)
}

impl Store {
    /// Called while the daemon holds its state lock. The outer error is a
    /// storage/decoding failure; the inner error is unavailable continuity.
    pub(crate) fn event_replay(
        &self,
        after: Option<&EventCursor>,
    ) -> Result<Result<EventReplay, String>> {
        let log: String = self.conn.query_row(
            "SELECT value FROM meta WHERE key='event_log_id'",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            cursor(&log, 0, "").is_valid(),
            "invalid durable event log identity"
        );
        let head_seq = self.max_event_seq()?;
        let start_seq = after.map_or(head_seq, |c| c.seq);
        if after.is_some_and(|c| !c.is_valid() || c.log != log || c.seq > head_seq) {
            return Ok(Err("event cursor is invalid, belongs to another database, or is ahead of durable history".into()));
        }
        let start = if start_seq == 0 {
            cursor(&log, 0, "")
        } else {
            // Check bytes before loading an arbitrarily large stored blob.
            let json: Option<String> = self
                .conn
                .query_row(
                    "SELECT json FROM events WHERE seq=?1 AND length(CAST(json AS BLOB)) < ?2",
                    params![start_seq as i64, EVENT_STREAM_FRAME_BYTES as i64],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(json) = json else {
                return Ok(Err(
                    "event cursor anchor was pruned or exceeds the replay bound".into(),
                ));
            };
            let event: Event = serde_json::from_str(&json)?;
            anyhow::ensure!(
                event.seq == start_seq,
                "stored event sequence disagrees with its row"
            );
            cursor(&log, start_seq, &json)
        };
        if after.is_some_and(|c| *c != start) {
            return Ok(Err(
                "event cursor anchor differs from retained history".into()
            ));
        }
        let (count, bytes): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(CAST(json AS BLOB))),0) FROM events WHERE seq > ?1 AND seq <= ?2",
            params![start_seq as i64, head_seq as i64], |row| Ok((row.get(0)?,row.get(1)?)),
        )?;
        if count as u64 != head_seq - start_seq
            || count > EVENT_REPLAY_LIMIT as i64
            || bytes >= EVENT_STREAM_FRAME_BYTES as i64
        {
            return Ok(Err(
                "event history has a gap or exceeds the bounded replay window".into(),
            ));
        }
        let mut stmt = self
            .conn
            .prepare("SELECT seq, json FROM events WHERE seq > ?1 AND seq <= ?2 ORDER BY seq")?;
        let rows = stmt.query_map(params![start_seq as i64, head_seq as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut frames = Vec::new();
        let mut head = start.clone();
        let mut frame_bytes = 0;
        for row in rows {
            let (seq, json) = row?;
            let event: Event = serde_json::from_str(&json)?;
            anyhow::ensure!(
                event.seq == seq as u64,
                "stored event sequence disagrees with its row"
            );
            if event.seq != head.seq + 1 {
                return Ok(Err("event history is not contiguous".into()));
            }
            head = cursor(&log, event.seq, &json);
            let frame = Response::EventAt {
                cursor: head.clone(),
                event,
            };
            frame_bytes += serde_json::to_vec(&frame)?.len() + 1;
            if frame_bytes > EVENT_STREAM_FRAME_BYTES {
                return Ok(Err("event replay exceeds the response byte bound".into()));
            }
            frames.push(frame);
        }
        Ok(Ok(EventReplay {
            start,
            head,
            frames,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::EventKind;

    fn append(store: &Store, seq: u64) -> Event {
        let mut event = Event::new(EventKind::WatcherStarted, Utc::now());
        event.seq = seq;
        store.append_event(&event).unwrap();
        event
    }

    #[test]
    fn pruning_all_events_preserves_high_water_across_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.db");
        let store = Store::open(&path).unwrap();
        append(&store, 41);
        let before = store.event_replay(None).unwrap().unwrap().head;
        assert_eq!(store.prune_events(0).unwrap(), 1);
        assert_eq!(store.max_event_seq().unwrap(), 41);
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.max_event_seq().unwrap(), 41);
        assert!(
            store.event_replay(None).unwrap().is_err(),
            "no anchor remains"
        );
        append(&store, store.max_event_seq().unwrap() + 1);
        let after = store.event_replay(None).unwrap().unwrap().head;
        assert_eq!(after.log, before.log);
        assert_eq!(after.seq, 42);
        assert!(store.event_replay(Some(&before)).unwrap().is_err());
        assert!(
            store
                .event_replay(Some(&after))
                .unwrap()
                .unwrap()
                .frames
                .is_empty()
        );
    }

    #[test]
    fn checked_replay_preserves_order_and_matches_live_cursors() {
        let store = Store::in_memory().unwrap();
        let initial = store.event_replay(None).unwrap().unwrap();
        assert_eq!(initial.start, initial.head);
        assert_eq!(initial.head.seq, 0);
        assert!(initial.head.is_valid());
        for seq in 1..=4 {
            let event = append(&store, seq);
            let snapshot = store.event_replay(Some(&initial.head)).unwrap().unwrap();
            assert_eq!(snapshot.frames.len(), seq as usize);
            let live = live_frame(&initial.head.log, event).unwrap();
            assert_eq!(
                serde_json::to_value(snapshot.frames.last().unwrap()).unwrap(),
                serde_json::to_value(live).unwrap()
            );
        }
        let other = Store::in_memory().unwrap();
        assert!(other.event_replay(Some(&initial.head)).unwrap().is_err());
    }

    #[test]
    fn changed_pruned_missing_and_future_cursors_never_claim_readiness() {
        let store = Store::in_memory().unwrap();
        append(&store, 1);
        let first = store.event_replay(None).unwrap().unwrap().head;
        append(&store, 2);
        append(&store, 3);
        let mut future = first.clone();
        future.seq = 4;
        assert!(store.event_replay(Some(&future)).unwrap().is_err());
        let mut invalid = first.clone();
        invalid.digest = "bad".into();
        assert!(store.event_replay(Some(&invalid)).unwrap().is_err());
        store.conn.execute("UPDATE events SET json=replace(json, 'watcher_started', 'watcher_stopped') WHERE seq=1", []).unwrap();
        assert!(store.event_replay(Some(&first)).unwrap().is_err());
        let revised = store
            .event_replay(Some(&EventCursor {
                seq: 0,
                digest: String::new(),
                ..first.clone()
            }))
            .unwrap()
            .unwrap()
            .frames
            .remove(0);
        let Response::EventAt {
            cursor: revised, ..
        } = revised
        else {
            panic!()
        };
        store
            .conn
            .execute("DELETE FROM events WHERE seq=2", [])
            .unwrap();
        assert!(
            store.event_replay(Some(&revised)).unwrap().is_err(),
            "interior gap"
        );
        store.prune_events(1).unwrap();
        assert!(
            store.event_replay(Some(&revised)).unwrap().is_err(),
            "pruned anchor"
        );
    }

    #[test]
    fn replay_bounds_and_storage_errors_are_explicit() {
        let store = Store::in_memory().unwrap();
        let initial = store.event_replay(None).unwrap().unwrap().head;
        for seq in 1..=EVENT_REPLAY_LIMIT as u64 {
            append(&store, seq);
        }
        assert!(store.event_replay(Some(&initial)).unwrap().is_ok());
        append(&store, EVENT_REPLAY_LIMIT as u64 + 1);
        assert!(store.event_replay(Some(&initial)).unwrap().is_err());
        store.conn.execute("DROP TABLE events", []).unwrap();
        assert!(
            store.event_replay(Some(&initial)).is_err(),
            "SQL failure is not empty replay"
        );
    }

    #[test]
    fn replay_byte_budget_and_row_integrity_are_checked_before_ready() {
        let store = Store::in_memory().unwrap();
        let initial = store.event_replay(None).unwrap().unwrap().head;
        append(&store, 1);
        store
            .conn
            .execute("UPDATE events SET json=json_set(json, '$.seq', 2)", [])
            .unwrap();
        assert!(store.event_replay(Some(&initial)).is_err());
        store
            .conn
            .execute(
                "UPDATE events SET json=?1",
                ["x".repeat(EVENT_STREAM_FRAME_BYTES)],
            )
            .unwrap();
        assert!(store.event_replay(Some(&initial)).unwrap().is_err());
        assert!(store.event_replay(None).unwrap().is_err());
    }
}
