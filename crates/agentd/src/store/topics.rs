//! The topic log and its readers' cursors. A topic message is kept here in
//! the transaction that sends it, for the project its sender is confined to
//! (`project`, NULL for anyone), under one sequence shared by every topic
//! that is never reused (AUTOINCREMENT), so a cursor or an `after_seq`
//! keeps its meaning after any prune. Retention is per topic and project:
//! one project's traffic on a topic never pushes another's out.
use super::*;
use agentdocker_core::topic::{self, ReplayBound, ReplayPlan, TopicCursor, TopicPost};

/// What one subscribe replays: the kept messages, oldest first, each with
/// its sequence, and how many past where the subscriber starts the bounds
/// left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TopicReplay {
    pub messages: Vec<(u64, Envelope)>,
    pub skipped: u64,
}

/// Who is replayed to, and from where.
pub(crate) struct ReplayRequest<'a> {
    /// The subscription's patterns.
    pub patterns: &'a [String],
    /// The project the reader is confined to ([`topic::scope`]).
    pub scope: Option<&'a ProjectId>,
    /// The agent whose cursors say where each topic starts, if any.
    pub reader: Option<&'a AgentId>,
    /// A point that replaces the cursors.
    pub after: Option<u64>,
    pub per_topic: usize,
    pub total: usize,
}

fn seq_of(raw: i64) -> u64 {
    u64::try_from(raw).unwrap_or(0)
}

fn sql_seq(seq: u64) -> i64 {
    i64::try_from(seq).unwrap_or(i64::MAX)
}

impl Store {
    /// Keep a topic message, inside the caller's transaction.
    pub(crate) fn keep_topic_message(
        &self,
        message: &Envelope,
        topic: &str,
        project: Option<ProjectId>,
    ) -> Result<TopicPost> {
        self.conn.execute(
            "INSERT INTO topic_messages (message_id, topic, project, sent_at, json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                message.id.as_str(),
                topic,
                project.as_ref().map(ProjectId::as_str),
                message.sent_at.to_rfc3339(),
                serde_json::to_string(message)?,
            ],
        )?;
        Ok(TopicPost {
            seq: seq_of(self.conn.last_insert_rowid()),
            project,
        })
    }

    /// The kept messages a subscription replays: for every kept topic its
    /// patterns match, the messages past where it starts there
    /// ([`topic::start`]) that its reader may see ([`topic::visible`]),
    /// chosen by [`Store::topic_replay_plan`]. Only the chosen messages'
    /// JSON is read.
    pub(crate) fn topic_replay(&self, request: &ReplayRequest<'_>) -> Result<TopicReplay> {
        let ReplayPlan { seqs, skipped, .. } = self.topic_replay_plan(request)?;
        let mut json = self
            .conn
            .prepare_cached("SELECT json FROM topic_messages WHERE seq = ?1")?;
        let mut messages = Vec::with_capacity(seqs.len());
        for seq in seqs {
            let text: String = json.query_row([sql_seq(seq)], |row| row.get(0))?;
            messages.push((seq, serde_json::from_str(&text)?));
        }
        Ok(TopicReplay { messages, skipped })
    }

    /// The sequences a subscription replays, chosen while the topics are
    /// read, one at a time: each kept topic name is found by one indexed
    /// seek past the last (never collected into a list), a matching topic's
    /// cursor by a point read, and its waiting messages are offered newest
    /// first to a [`ReplayBound`], which stops reading that topic as soon as
    /// nothing more of it could be kept. At most `total` candidate
    /// sequences are held at once, whatever the number of topics; per
    /// matching topic the work is one count and at most `per_topic` rows.
    pub(crate) fn topic_replay_plan(&self, request: &ReplayRequest<'_>) -> Result<ReplayPlan> {
        if request.patterns.is_empty()
            || topic::start(request.reader.is_some(), None, request.after).is_none()
        {
            return Ok(ReplayPlan::default());
        }
        let mut first = self
            .conn
            .prepare_cached("SELECT topic FROM topic_messages ORDER BY topic LIMIT 1")?;
        let mut next = self.conn.prepare_cached(
            "SELECT topic FROM topic_messages WHERE topic > ?1 ORDER BY topic LIMIT 1",
        )?;
        let mut cursor = self
            .conn
            .prepare_cached("SELECT seq FROM topic_cursors WHERE agent = ?1 AND topic = ?2")?;
        let mut count = self.conn.prepare_cached(
            "SELECT COUNT(*) FROM topic_messages
             WHERE topic = ?1 AND seq > ?2 AND (?3 IS NULL OR project IS NULL OR project = ?3)",
        )?;
        let mut newest = self.conn.prepare_cached(
            "SELECT seq FROM topic_messages
             WHERE topic = ?1 AND seq > ?2 AND (?3 IS NULL OR project IS NULL OR project = ?3)
             ORDER BY seq DESC LIMIT ?4",
        )?;
        let scope = request.scope.map(ProjectId::as_str);
        let per_topic = i64::try_from(request.per_topic).unwrap_or(i64::MAX);
        let mut bound = ReplayBound::new(request.per_topic, request.total);
        let mut current: Option<String> = first.query_row([], |row| row.get(0)).optional()?;
        while let Some(topic) = current {
            let matching = request
                .patterns
                .iter()
                .any(|pattern| agentdocker_core::topic_matches(pattern, &topic));
            let held = match (matching, request.reader) {
                (true, Some(reader)) => cursor
                    .query_row(params![reader.as_str(), topic], |row| row.get::<_, i64>(0))
                    .optional()?
                    .map(seq_of),
                _ => None,
            };
            let start = matching
                .then(|| topic::start(request.reader.is_some(), held, request.after))
                .flatten();
            if let Some(start) = start.map(sql_seq) {
                let pending: i64 =
                    count.query_row(params![topic, start, scope], |row| row.get(0))?;
                if pending > 0 {
                    bound.topic(seq_of(pending));
                    let mut rows = newest.query(params![topic, start, scope, per_topic])?;
                    while let Some(row) = rows.next()? {
                        if !bound.offer(seq_of(row.get(0)?)) {
                            break;
                        }
                    }
                }
            }
            current = next.query_row([&topic], |row| row.get(0)).optional()?;
        }
        Ok(bound.finish())
    }

    /// Where `ids` stand in the topic log, for those that are kept topic
    /// messages: id, topic and place.
    pub(crate) fn topic_posts(
        &self,
        ids: &[MessageId],
    ) -> Result<Vec<(MessageId, String, TopicPost)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT topic, seq, project FROM topic_messages WHERE message_id = ?1",
        )?;
        let mut found = Vec::new();
        for id in ids {
            let row = stmt
                .query_row([id.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .optional()?;
            if let Some((topic, seq, project)) = row {
                found.push((
                    id.clone(),
                    topic,
                    TopicPost {
                        seq: seq_of(seq),
                        project: project.as_deref().map(ProjectId::from),
                    },
                ));
            }
        }
        Ok(found)
    }

    /// Every topic cursor `agent` holds.
    pub(crate) fn topic_cursors(&self, agent: &AgentId) -> Result<Vec<(String, u64)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT topic, seq FROM topic_cursors WHERE agent = ?1")?;
        let rows = stmt
            .query_map([agent.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1).map(seq_of)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Move `agent`'s cursors with the event that says so, in one
    /// transaction. A cursor only moves forward, here as in the plan.
    pub(crate) fn advance_topic_cursors(
        &self,
        agent: &AgentId,
        cursors: &[TopicCursor],
        event: &Event,
    ) -> Result<()> {
        let tx = write_transaction(&self.conn)?;
        for cursor in cursors {
            self.conn.execute(
                "INSERT INTO topic_cursors (agent, topic, seq, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(agent, topic) DO UPDATE SET
                     seq = MAX(seq, excluded.seq),
                     updated_at = excluded.updated_at",
                params![
                    agent.as_str(),
                    cursor.topic,
                    sql_seq(cursor.seq),
                    event.at.to_rfc3339()
                ],
            )?;
        }
        self.append_event(event)?;
        tx.commit()?;
        Ok(())
    }

    /// A retired identity's cursors become the one that stays, each topic
    /// at the further of the two, inside the caller's transaction.
    pub(crate) fn merge_topic_cursors(&self, kept: &AgentId, retired: &AgentId) -> Result<()> {
        self.conn.execute(
            "INSERT INTO topic_cursors (agent, topic, seq, updated_at)
             SELECT ?1, topic, seq, updated_at FROM topic_cursors WHERE agent = ?2
             ON CONFLICT(agent, topic) DO UPDATE SET
                 seq = MAX(seq, excluded.seq),
                 updated_at = MAX(updated_at, excluded.updated_at)",
            params![kept.as_str(), retired.as_str()],
        )?;
        self.conn.execute(
            "DELETE FROM topic_cursors WHERE agent = ?1",
            [retired.as_str()],
        )?;
        Ok(())
    }

    /// Drop kept topic messages older than `cutoff`, and the oldest beyond
    /// `limit` per topic and project, within one budget of `batch` rows for
    /// the whole tick, with the exact-count event, or neither.
    pub(crate) fn prune_topic_messages_with_event(
        &self,
        cutoff: Option<DateTime<Utc>>,
        limit: usize,
        batch: usize,
        seq: u64,
        now: DateTime<Utc>,
    ) -> Result<Option<Event>> {
        let tx = write_transaction(&self.conn)?;
        let removed = self.prune_topic_messages(cutoff, limit, batch)?;
        let event = (removed > 0).then(|| {
            let mut event = Event::new(EventKind::TopicMessagesPruned { removed }, now);
            event.seq = seq;
            event
        });
        if let Some(event) = &event {
            self.append_event(event)?;
        }
        tx.commit()?;
        Ok(event)
    }

    fn prune_topic_messages(
        &self,
        cutoff: Option<DateTime<Utc>>,
        limit: usize,
        batch: usize,
    ) -> Result<usize> {
        let mut budget = i64::try_from(batch).unwrap_or(i64::MAX);
        let mut doomed: Vec<i64> = Vec::new();
        if let Some(cutoff) = cutoff
            && budget > 0
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT seq FROM topic_messages WHERE sent_at < ?1 ORDER BY seq LIMIT ?2",
            )?;
            let old: Vec<i64> = stmt
                .query_map(params![cutoff.to_rfc3339(), budget], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            budget -= i64::try_from(old.len()).unwrap_or(i64::MAX);
            doomed.extend(old);
        }
        let mut removed = self.delete_topic_messages(&doomed)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let crowded: Vec<(String, Option<String>, i64)> = {
            let mut over = self.conn.prepare_cached(
                "SELECT topic, project, COUNT(*) FROM topic_messages
                 GROUP BY topic, project HAVING COUNT(*) > ?1 ORDER BY topic, project",
            )?;
            over.query_map([limit], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        for (topic, project, count) in crowded {
            if budget <= 0 {
                break;
            }
            let excess = (count - limit).min(budget);
            let mut stmt = self.conn.prepare_cached(
                "SELECT seq FROM topic_messages WHERE topic = ?1 AND project IS ?2
                 ORDER BY seq LIMIT ?3",
            )?;
            let oldest: Vec<i64> = stmt
                .query_map(params![topic, project, excess], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            let went = self.delete_topic_messages(&oldest)?;
            removed += went;
            budget -= i64::try_from(went).unwrap_or(i64::MAX);
        }
        Ok(removed)
    }

    fn delete_topic_messages(&self, seqs: &[i64]) -> Result<usize> {
        let mut delete = self
            .conn
            .prepare_cached("DELETE FROM topic_messages WHERE seq = ?1")?;
        let mut removed = 0;
        for seq in seqs {
            removed += delete.execute([seq])?;
        }
        Ok(removed)
    }

    /// Every kept message's sequence.
    #[cfg(test)]
    pub(crate) fn kept_seqs(&self) -> Result<Vec<u64>> {
        let mut stmt = self.conn.prepare("SELECT seq FROM topic_messages")?;
        let seqs = stmt
            .query_map([], |row| row.get::<_, i64>(0).map(seq_of))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(seqs)
    }

    /// How many messages are kept on `topic`, for any project.
    #[cfg(test)]
    pub(crate) fn kept_on_topic(&self, topic: &str) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM topic_messages WHERE topic = ?1",
            [topic],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{Destination, topic_matches};
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    const TOPICS: [&str; 4] = ["a/x", "a/y", "b/x", "a"];
    const PROJECTS: [Option<&str>; 3] = [None, Some("alpha"), Some("beta")];
    const PATTERNS: [&[&str]; 4] = [&["#"], &["a/+"], &["+/x"], &["a", "b/#"]];
    /// Readers 0–2 are agents confined to PROJECTS[reader]; reader 3 is a
    /// subscriber with no agent.
    const READERS: usize = 4;
    const ANONYMOUS: usize = 3;
    const PER_TOPIC: usize = 3;
    const TOTAL: usize = 5;

    #[derive(Clone, Debug)]
    enum Op {
        Send {
            topic: usize,
            project: usize,
        },
        Ack {
            reader: usize,
            pick: usize,
        },
        Prune {
            limit: usize,
            age: i64,
            batch: usize,
        },
        Reopen,
        Replay {
            reader: usize,
            patterns: usize,
            after: Option<usize>,
        },
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0..TOPICS.len(), 0..PROJECTS.len())
                .prop_map(|(topic, project)| Op::Send { topic, project }),
            3 => (0..ANONYMOUS, 0..64usize).prop_map(|(reader, pick)| Op::Ack { reader, pick }),
            1 => (1..4usize, 0..12i64, 1..6usize)
                .prop_map(|(limit, age, batch)| Op::Prune { limit, age, batch }),
            1 => Just(Op::Reopen),
            3 => (0..READERS, 0..PATTERNS.len(), proptest::option::of(0..64usize))
                .prop_map(|(reader, patterns, after)| Op::Replay { reader, patterns, after }),
        ]
    }

    #[derive(Clone, Debug)]
    struct Kept {
        seq: u64,
        id: MessageId,
        topic: usize,
        project: usize,
        at: i64,
    }

    fn project_id(project: usize) -> Option<ProjectId> {
        PROJECTS[project].map(ProjectId::from)
    }

    fn reader_scope(reader: usize) -> Option<ProjectId> {
        (reader != ANONYMOUS).then(|| project_id(reader)).flatten()
    }

    fn visible_to(kept: &Kept, reader: usize) -> bool {
        match (PROJECTS[kept.project], reader_scope(reader)) {
            (Some(kept_for), Some(scope)) => kept_for == scope.as_str(),
            _ => true,
        }
    }

    fn reader_id(reader: usize) -> AgentId {
        AgentId::from(format!("reader-{reader}"))
    }

    /// The replay the model expects, worked out without the plan.
    fn expected(
        kept: &[Kept],
        cursors: &BTreeMap<String, u64>,
        reader: usize,
        patterns: &[&str],
        after: Option<u64>,
    ) -> (Vec<u64>, u64) {
        let agent = reader != ANONYMOUS;
        let mut candidates = Vec::new();
        let mut skipped = 0u64;
        for (topic, name) in TOPICS.iter().enumerate() {
            if !patterns.iter().any(|p| topic_matches(p, name)) {
                continue;
            }
            let start = match (after, agent) {
                (Some(after), _) => after,
                (None, true) => cursors.get(*name).copied().unwrap_or(0),
                (None, false) => continue,
            };
            let mut pending: Vec<u64> = kept
                .iter()
                .filter(|k| k.topic == topic && k.seq > start && visible_to(k, reader))
                .map(|k| k.seq)
                .collect();
            pending.sort_unstable_by(|a, b| b.cmp(a));
            skipped += pending.len().saturating_sub(PER_TOPIC) as u64;
            candidates.extend(pending.into_iter().take(PER_TOPIC));
        }
        candidates.sort_unstable_by(|a, b| b.cmp(a));
        skipped += candidates.len().saturating_sub(TOTAL) as u64;
        candidates.truncate(TOTAL);
        candidates.sort_unstable();
        (candidates, skipped)
    }

    /// Retention as the model has it: the oldest past the age, then the
    /// oldest of each crowded topic and project in order, within one
    /// budget.
    fn prune(kept: &mut Vec<Kept>, cutoff: i64, limit: usize, batch: usize) -> usize {
        let mut budget = batch;
        let mut doomed: Vec<u64> = kept
            .iter()
            .filter(|k| k.at < cutoff)
            .map(|k| k.seq)
            .take(budget)
            .collect();
        budget -= doomed.len();
        kept.retain(|k| !doomed.contains(&k.seq));
        let mut partitions: BTreeMap<(&str, Option<&str>), Vec<u64>> = BTreeMap::new();
        for k in kept.iter() {
            partitions
                .entry((TOPICS[k.topic], PROJECTS[k.project]))
                .or_default()
                .push(k.seq);
        }
        for seqs in partitions.values() {
            if budget == 0 {
                break;
            }
            if seqs.len() > limit {
                let excess = (seqs.len() - limit).min(budget);
                doomed.extend(seqs.iter().take(excess));
                budget -= excess;
            }
        }
        kept.retain(|k| !doomed.contains(&k.seq));
        doomed.len()
    }

    /// Many topics, each with more waiting than its share: one subscribe
    /// to `#` never holds more than its total in candidates while it reads
    /// them, for an agent with no cursors and for a watcher with no agent
    /// replaying from the start alike, and still replays the newest.
    #[test]
    fn a_replay_across_many_topics_holds_no_more_than_its_total() {
        const TOPIC_COUNT: usize = 300;
        const EACH: usize = 20;
        const SHARE: usize = 10;
        const CAP: usize = 50;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let base = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut all = Vec::new();
        {
            let tx = write_transaction(&store.conn).unwrap();
            // Round by round, so the newest are spread over every topic.
            for round in 0..EACH {
                for topic in 0..TOPIC_COUNT {
                    let name = format!("feed/{topic:03}");
                    let message = Envelope::new(
                        "sender",
                        Destination::Topic(name.clone()),
                        "notice",
                        serde_json::json!({ "round": round }),
                        None,
                        base,
                    );
                    all.push(store.keep_topic_message(&message, &name, None).unwrap().seq);
                }
            }
            tx.commit().unwrap();
        }
        let patterns = vec!["#".to_owned()];
        let reader = AgentId::from("reader");
        for (reader, after) in [(Some(&reader), None), (None, Some(0))] {
            let plan = store
                .topic_replay_plan(&ReplayRequest {
                    patterns: &patterns,
                    scope: None,
                    reader,
                    after,
                    per_topic: SHARE,
                    total: CAP,
                })
                .unwrap();
            assert!(plan.peak <= CAP, "held {} candidates at once", plan.peak);
            assert_eq!(plan.seqs, all[all.len() - CAP..]);
            assert_eq!(plan.skipped, (TOPIC_COUNT * EACH - CAP) as u64);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Sends, acknowledgements, retention, restarts and replays, in any
        /// order, keep the log, every cursor and every replay as the model
        /// says: nothing a reader could see and has not acknowledged past
        /// is lost to it except by retention or the bounds it is told of,
        /// nothing it cannot see reaches it, and a cursor never moves back.
        #[test]
        fn topic_log_sequences_match_the_model(ops in prop::collection::vec(op(), 1..80)) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.db");
            let mut store = Store::open(&path).unwrap();
            let base = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
            let mut clock = 0i64;
            let mut kept: Vec<Kept> = Vec::new();
            let mut sent: Vec<Kept> = Vec::new();
            let mut cursors: Vec<BTreeMap<String, u64>> = vec![BTreeMap::new(); ANONYMOUS];
            let mut last_seq = 0u64;
            for op in ops {
                clock += 1;
                let now = base + chrono::Duration::seconds(clock);
                match op {
                    Op::Send { topic, project } => {
                        let message = Envelope::new(
                            "sender",
                            Destination::Topic(TOPICS[topic].to_owned()),
                            "notice",
                            serde_json::json!({ "n": clock }),
                            None,
                            now,
                        );
                        let post = store
                            .keep_topic_message(&message, TOPICS[topic], project_id(project))
                            .unwrap();
                        prop_assert!(post.seq > last_seq, "never reused");
                        last_seq = post.seq;
                        prop_assert_eq!(post.project, project_id(project));
                        let entry = Kept { seq: post.seq, id: message.id, topic, project, at: clock };
                        kept.push(entry.clone());
                        sent.push(entry);
                    }
                    Op::Ack { reader, pick } => {
                        // Anything ever sent, pruned or not, kept for
                        // anyone: the daemon's acknowledgement path.
                        let Some(target) = sent.get(pick % sent.len().max(1)).cloned() else {
                            continue;
                        };
                        let id = reader_id(reader);
                        let scope = reader_scope(reader);
                        let posts = store.topic_posts(std::slice::from_ref(&target.id)).unwrap();
                        let acknowledged: Vec<(String, u64)> = posts
                            .into_iter()
                            .filter(|(_, _, post)| topic::visible(post.project.as_ref(), scope.as_ref()))
                            .map(|(_, topic, post)| (topic, post.seq))
                            .collect();
                        let current: HashMap<String, u64> =
                            store.topic_cursors(&id).unwrap().into_iter().collect();
                        let moved = topic::advance(|t| current.get(t).copied(), acknowledged);
                        if !moved.is_empty() {
                            let event = Event::new(
                                EventKind::TopicCursorsAdvanced { agent: id.clone(), cursors: moved.clone() },
                                now,
                            );
                            store.advance_topic_cursors(&id, &moved, &event).unwrap();
                        }
                        let still_kept = kept.iter().any(|k| k.seq == target.seq);
                        if still_kept && visible_to(&target, reader) {
                            let cursor = cursors[reader].entry(TOPICS[target.topic].to_owned()).or_insert(0);
                            prop_assert_eq!(moved.is_empty(), *cursor >= target.seq);
                            *cursor = (*cursor).max(target.seq);
                        } else {
                            prop_assert!(moved.is_empty(), "a pruned or unseen message moves nothing");
                        }
                    }
                    Op::Prune { limit, age, batch } => {
                        let cutoff = clock - age;
                        let removed = prune(&mut kept, cutoff, limit, batch);
                        let event = store
                            .prune_topic_messages_with_event(
                                Some(base + chrono::Duration::seconds(cutoff)),
                                limit,
                                batch,
                                0,
                                now,
                            )
                            .unwrap();
                        match event.map(|e| e.kind) {
                            Some(EventKind::TopicMessagesPruned { removed: said }) => {
                                prop_assert_eq!(said, removed);
                            }
                            None => prop_assert_eq!(removed, 0),
                            other => prop_assert!(false, "{other:?}"),
                        }
                    }
                    Op::Reopen => {
                        drop(store);
                        store = Store::open(&path).unwrap();
                    }
                    Op::Replay { reader, patterns, after } => {
                        let after = after.and_then(|pick| sent.get(pick % sent.len().max(1))).map(|k| k.seq);
                        let id = reader_id(reader);
                        let scope = reader_scope(reader);
                        let patterns: Vec<String> = PATTERNS[patterns].iter().map(|p| (*p).to_owned()).collect();
                        let replay = store
                            .topic_replay(&ReplayRequest {
                                patterns: &patterns,
                                scope: scope.as_ref(),
                                reader: (reader != ANONYMOUS).then_some(&id),
                                after,
                                per_topic: PER_TOPIC,
                                total: TOTAL,
                            })
                            .unwrap();
                        let model = if reader == ANONYMOUS { BTreeMap::new() } else { cursors[reader].clone() };
                        let names: Vec<&str> = patterns.iter().map(String::as_str).collect();
                        let (seqs, skipped) = expected(&kept, &model, reader, &names, after);
                        let got: Vec<u64> = replay.messages.iter().map(|(seq, _)| *seq).collect();
                        prop_assert_eq!(got, seqs);
                        prop_assert_eq!(replay.skipped, skipped);
                        for (seq, message) in &replay.messages {
                            let k = kept.iter().find(|k| k.seq == *seq).unwrap();
                            prop_assert_eq!(&message.id, &k.id);
                        }
                    }
                }
                let mut stored = store.kept_seqs().unwrap();
                stored.sort_unstable();
                prop_assert_eq!(stored, kept.iter().map(|k| k.seq).collect::<Vec<_>>());
                for (reader, model) in cursors.iter().enumerate() {
                    let held: BTreeMap<String, u64> =
                        store.topic_cursors(&reader_id(reader)).unwrap().into_iter().collect();
                    prop_assert_eq!(&held, model);
                }
            }
        }
    }
}
