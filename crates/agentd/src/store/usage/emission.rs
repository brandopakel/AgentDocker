//! Successful-write metadata shares the bounded accounting store. Keeping IDs
//! after retention prevents a delayed duplicate from resurrecting old totals.
use super::*;
use agentdocker_core::usage::emission::Emission;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Recorded {
    New,
    Duplicate,
    Conflict,
    Capacity,
}

impl Store {
    pub(crate) fn record_emission(
        &self,
        agent: &AgentId,
        project: Option<&str>,
        emission: &Emission,
        event: &Event,
    ) -> Result<Recorded> {
        emission.check().map_err(anyhow::Error::msg)?;
        let bytes =
            i64::try_from(emission.bytes).context("emission bytes exceed SQLite integer range")?;
        let tx = write_transaction(&self.conn)?;
        let previous: Option<(String, String, i64)> = self
            .conn
            .query_row(
                "SELECT agent,channel,bytes FROM usage_emissions WHERE id=?1",
                [&emission.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let channel = serde_json::to_string(&emission.channel)?;
        if let Some(previous) = previous {
            return Ok(if previous == (agent.to_string(), channel, bytes) {
                Recorded::Duplicate
            } else {
                Recorded::Conflict
            });
        }
        if self
            .usage_bounded_write(|| {
                self.conn.execute(
                    "INSERT INTO usage_emissions VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        emission.id,
                        agent.as_str(),
                        project,
                        channel,
                        bytes,
                        text(event.at)
                    ],
                )?;
                Ok(())
            })?
            .is_none()
        {
            return Ok(Recorded::Capacity);
        }
        self.append_event(event)?;
        tx.commit()?;
        Ok(Recorded::New)
    }

    pub(crate) fn emission_report(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        project: Option<&str>,
        agent: Option<&str>,
    ) -> Result<Overhead> {
        let (known_events, injected_bytes) = self.conn.query_row(
            "SELECT COUNT(*), SUM(bytes) FROM usage_emissions
             WHERE at>=?1 AND at<?2 AND (?3 IS NULL OR project=?3) AND (?4 IS NULL OR agent=?4)",
            params![text(since), text(until), project, agent],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)),
        )?;
        let known_events = u64::try_from(known_events).context("negative emission count")?;
        let injected_bytes = injected_bytes
            .map(u64::try_from)
            .transpose()
            .context("negative emitted-byte sum")?;
        Ok(Overhead {
            injected_bytes,
            known_events,
            // Output and this report cannot commit atomically. Crashes, old
            // writers, disabled collection and reporting failures leave gaps.
            coverage: if known_events == 0 {
                usage::Coverage::Unknown
            } else {
                usage::Coverage::Partial
            },
            estimated_tokens: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::usage::emission::Channel;

    fn at(hour: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-09-17T{hour:02}:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    fn record(
        store: &Store,
        id: u32,
        agent: &str,
        project: Option<&str>,
        bytes: u64,
        hour: u32,
    ) -> Result<Recorded> {
        let emission = Emission {
            id: format!("{id:032x}"),
            channel: Channel::Mcp,
            bytes,
        };
        let agent = AgentId::from(agent.to_owned());
        let mut event = Event::new(
            EventKind::UsageEmissionRecorded {
                agent: agent.clone(),
                emission: emission.clone(),
            },
            at(hour),
        );
        event.seq = u64::from(id);
        store.record_emission(&agent, project, &emission, &event)
    }

    #[test]
    fn writes_are_idempotent_filtered_and_not_provider_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        assert_eq!(
            record(&store, 1, "a", Some("p"), 12, 1).unwrap(),
            Recorded::New
        );
        assert_eq!(
            record(&store, 1, "a", Some("changed-project"), 12, 2).unwrap(),
            Recorded::Duplicate
        );
        assert_eq!(
            record(&store, 1, "a", Some("p"), 13, 1).unwrap(),
            Recorded::Conflict
        );
        assert_eq!(
            record(&store, 1, "b", Some("p"), 12, 1).unwrap(),
            Recorded::Conflict
        );
        assert_eq!(
            record(&store, 2, "b", Some("q"), 24, 2).unwrap(),
            Recorded::New
        );
        let report = store.emission_report(at(0), at(3), None, None).unwrap();
        assert_eq!((report.injected_bytes, report.known_events), (Some(36), 2));
        assert_eq!(report.coverage, usage::Coverage::Partial);
        assert_eq!(
            store
                .emission_report(at(0), at(3), Some("p"), Some("a"))
                .unwrap()
                .injected_bytes,
            Some(12)
        );
        assert_eq!(
            store
                .emission_report(at(0), at(3), Some("p"), Some("b"))
                .unwrap(),
            Overhead::default()
        );
        assert_eq!(
            store
                .emission_report(at(2), at(3), None, None)
                .unwrap()
                .injected_bytes,
            Some(24)
        );
        let range = Range::new(Some(at(0)), Some(at(3)), at(4), at(0)).unwrap();
        let usage = store
            .usage_report(range, Group::Agent, None, None)
            .unwrap()
            .unwrap();
        assert!(usage.rows.is_empty());
        assert_eq!(usage.overhead, report);
        drop(store);
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        assert_eq!(
            record(&store, 1, "a", Some("p"), 12, 3).unwrap(),
            Recorded::Duplicate
        );
        assert_eq!(
            store.emission_report(at(0), at(3), None, None).unwrap(),
            report
        );
        store.prune_usage(at(2)).unwrap();
        assert_eq!(
            record(&store, 1, "a", Some("p"), 12, 3).unwrap(),
            Recorded::Duplicate
        );
        let range = Range::new(Some(at(0)), Some(at(3)), at(4), at(0)).unwrap();
        let report = store
            .usage_report(range, Group::Hour, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            (report.overhead.injected_bytes, report.overhead.known_events),
            (Some(24), 1)
        );
        assert!(report.coverage.history_truncated);
    }

    #[test]
    fn capacity_and_event_failure_leave_no_emission_or_changed_byte_total() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        store
            .conn
            .execute("UPDATE usage_tracking SET capacity=0", [])
            .unwrap();
        assert_eq!(
            record(&store, 1, "a", None, 8, 1).unwrap(),
            Recorded::Capacity
        );
        store
            .conn
            .execute("UPDATE usage_tracking SET capacity=268435456", [])
            .unwrap();
        store.reject_event_for_test("usage_emission_recorded");
        assert!(record(&store, 1, "a", None, 8, 1).is_err());
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM usage_emissions", [], |r| r.get(0))
            .unwrap();
        let bytes: i64 = store
            .conn
            .query_row("SELECT bytes FROM usage_tracking", [], |r| r.get(0))
            .unwrap();
        assert_eq!((count, bytes), (0, 0));
    }
}
