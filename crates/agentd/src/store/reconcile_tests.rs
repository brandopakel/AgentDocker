use super::*;
use agentdocker_core::{
    AgentSpec, AgentStatus, Destination, EventKind, LeaseMode, ProjectRef, ResourceKey,
};
use chrono::Duration;
use serde_json::json;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-11T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn record(id: &str) -> AgentRecord {
    let mut record = AgentRecord::new(
        AgentSpec {
            name: id.into(),
            runtime: "claude-code".into(),
            workdir: Some("/fixture/checkout".into()),
            ..Default::default()
        },
        false,
        now(),
    );
    record.id = id.into();
    record.pid = Some(123);
    record.process_started_at = Some(now());
    record.project = Some(ProjectRef::directory("/fixture/checkout"));
    record
        .spec
        .labels
        .insert("session_id".into(), "same-session".into());
    record.status = AgentStatus::Exited { code: Some(0) };
    record.finished_at = Some(now());
    record
}

fn seed(store: &Store) -> (AgentId, AgentId) {
    let a = record("canonical");
    let mut b = record("retired");
    b.spec.labels.remove("session_id");
    store.upsert_agent(&a).unwrap();
    store.upsert_agent(&b).unwrap();
    store
        .conn
        .execute("UPDATE meta SET value='10' WHERE key='schema_version'", [])
        .unwrap();
    (a.id, b.id)
}

fn envelope(id: &str, to: &AgentId) -> Envelope {
    let mut message = Envelope::new(
        "retired",
        Destination::Agent(to.clone()),
        "task",
        json!({"text":"retired", "opaque_agent_reference":"retired"}),
        None,
        now(),
    );
    message.id = id.to_owned().into();
    message
}

fn preview(store: &Store, a: &AgentId, b: &AgentId) -> RepairPreview {
    store
        .repair(a, b, None, now(), |_| {
            panic!("preview must not check or affect processes")
        })
        .unwrap()
}

fn snapshot(store: &Store) -> Vec<Vec<String>> {
    [
        "agents",
        "leases",
        "inbox",
        "documents",
        "events",
        "journal",
        "changes",
    ]
    .iter()
    .map(|table| {
        let mut statement = store
            .conn
            .prepare(&format!("SELECT json FROM {table} ORDER BY json"))
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn repaired_hook_and_mcp_duplicates_preserve_both_provenance_records() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("state.db")).unwrap();
    let (a, b) = seed(&store);
    let mut kept = record(a.as_str());
    kept.spec.labels.insert("via".into(), "hook".into());
    let mut retired = record(b.as_str());
    retired.spec.labels.remove("session_id");
    retired.spec.labels.insert("via".into(), "mcp".into());
    store.upsert_agent(&kept).unwrap();
    store.upsert_agent(&retired).unwrap();
    let before = snapshot(&store);
    let plan = preview(&store, &a, &b);
    assert_eq!(snapshot(&store), before);
    store
        .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| Ok(()))
        .unwrap();
    let remaining = store.load_agents().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].spec.labels["via"], "hook");
    let archive = store
        .document::<Value>("identity_reconciliation", b.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(
        archive["before"]["canonical"],
        serde_json::to_value(kept).unwrap()
    );
    assert_eq!(
        archive["before"]["retired"],
        serde_json::to_value(retired).unwrap()
    );
    assert_eq!(store.identity_aliases().unwrap()[0].canonical, a);
    assert_eq!(
        store.identity_aliases().unwrap()[0].retired_name.as_deref(),
        Some("retired")
    );
    drop(store);
    let store = Store::open(&tmp.path().join("state.db")).unwrap();
    assert_eq!(
        store.identity_aliases().unwrap()[0].retired_name.as_deref(),
        Some("retired")
    );
}

#[test]
fn repair_keeps_fifo_payloads_history_and_before_images() {
    let store = Store::in_memory().unwrap();
    let (a, b) = seed(&store);
    let first = envelope("first", &b);
    let second = envelope("second", &a);
    store.enqueue(&b, &first, 1000).unwrap();
    store.enqueue(&a, &second, 1000).unwrap();
    store.enqueue(&a, &first, 1000).unwrap(); // identical broadcast copy only
    let project = record("canonical").project.unwrap().id();
    let question = agentdocker_core::Question {
        presentation: None,
        id: "question".to_owned().into(),
        from: b.to_string(),
        to: Destination::Agent("human".into()),
        text: "retired".into(),
        asked_at: now(),
        expires_at: now() + Duration::days(1),
    };
    store
        .put_document("question", "question", &question)
        .unwrap();
    let reads = vec![ReadMark {
        path: "/fixture/checkout/file".into(),
        at: now(),
        version: "abc".into(),
        head: Some("head".into()),
    }];
    store.put_document("reads", b.as_str(), &reads).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO journal_cursors VALUES (?1,?2,8,?3)",
            params![a.as_str(), project.as_str(), now().to_rfc3339()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO journal_cursors VALUES (?1,?2,3,?3)",
            params![b.as_str(), project.as_str(), now().to_rfc3339()],
        )
        .unwrap();
    for (seq, id) in [(1, &a), (2, &b)] {
        let entry = JournalEntry {
            project: project.clone(),
            seq,
            at: now(),
            agent: Some(id.clone()),
            agent_name: id.to_string(),
            branch: None,
            checkout: None,
            worktree: None,
            kind: agentdocker_core::JournalKind::Note,
            summary: "retired".into(),
            summary_source: agentdocker_core::SummarySource::Explicit,
            resources: vec![],
            paths: vec![],
            paths_total: 0,
            head_before: None,
            head_after: None,
            changes: None,
        };
        store.append_journal(&entry).unwrap();
        store
            .append_change(&Change {
                seq: 0,
                project: project.clone(),
                checkout: None,
                worktree: None,
                path: "file".into(),
                kind: agentdocker_core::ChangeKind::Modified,
                at: now(),
                by: agentdocker_core::Attribution::Agent {
                    agent: id.clone(),
                    lease: "lease".into(),
                    note: None,
                },
                head: None,
            })
            .unwrap();
    }
    let before = snapshot(&store);
    let plan = preview(&store, &a, &b);
    assert!(!plan.applied);
    assert_eq!(snapshot(&store), before);
    let result = store
        .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| Ok(()))
        .unwrap();
    assert!(result.applied);
    assert_eq!(store.load_agents().unwrap().len(), 1);
    assert_eq!(
        store.load_inboxes().unwrap()[&a]
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![first, second]
    );
    assert_eq!(store.journal_cursor(a.as_str(), &project).unwrap(), Some(3));
    assert_eq!(store.journal_cursor(b.as_str(), &project).unwrap(), None);
    assert_eq!(
        store
            .document::<Vec<ReadMark>>("reads", a.as_str())
            .unwrap(),
        Some(reads)
    );
    let moved = store
        .document::<agentdocker_core::Question>("question", "question")
        .unwrap()
        .unwrap();
    assert_eq!(moved.from, a.as_str());
    assert_eq!(moved.text, "retired");
    for id in [&a, &b] {
        let mut query = JournalQuery::new(project.clone(), 10);
        query.agent = Some(id.clone());
        let history = store.journal(&query).unwrap();
        assert_eq!(
            history
                .iter()
                .map(|e| e.agent.clone().unwrap())
                .collect::<Vec<_>>(),
            vec![a.clone(), b.clone()]
        );
        let changes = store
            .changes(&ChangesQuery {
                project: project.clone(),
                since_seq: None,
                path: None,
                agent: Some(id.clone()),
                limit: 10,
                after: None,
                before_seq: None,
            })
            .unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[1].by.agent(), Some(&b));
    }
    let after = snapshot(&store);
    assert_eq!(before[5..], after[5..], "historical rows are not rewritten");
    let archive = store
        .document::<Value>("identity_reconciliation", b.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(archive["before"]["retired"]["id"], b.as_str());
    assert_eq!(
        archive["before"]["documents"][0]["value"]["from"],
        b.as_str()
    );
    assert!(matches!(
        store.recent_events(1).unwrap()[0].kind,
        EventKind::AgentReconciled { .. }
    ));
    assert!(
        store
            .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| panic!(
                "idempotent"
            ))
            .unwrap()
            .applied
    );
    assert_eq!(
        snapshot(&store),
        after,
        "reapplying must not emit or move anything"
    );
}

#[test]
fn changed_plan_and_failed_commit_leave_all_state_untouched() {
    let store = Store::in_memory().unwrap();
    let (a, b) = seed(&store);
    let plan = preview(&store, &a, &b);
    store.enqueue(&b, &envelope("late", &b), 1000).unwrap();
    let before = snapshot(&store);
    let err = store
        .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| {
            panic!("digest first")
        })
        .unwrap_err();
    assert!(err.to_string().contains("changed"));
    assert_eq!(snapshot(&store), before);
    let plan = preview(&store, &a, &b);
    store.conn.execute_batch("CREATE TRIGGER reject_repair BEFORE INSERT ON events BEGIN SELECT RAISE(FAIL,'injected event failure'); END;").unwrap();
    assert!(
        store
            .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| Ok(()))
            .is_err()
    );
    assert_eq!(snapshot(&store), before);
    let schema: String = store
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema, "10", "schema changes roll back too");
    assert_eq!(preview(&store, &a, &b).plan_sha256, plan.plan_sha256);
}

#[test]
fn mismatched_database_keys_cannot_authorize_removing_another_identity() {
    let store = Store::in_memory().unwrap();
    let (a, b) = seed(&store);
    store
        .conn
        .execute(
            "UPDATE agents SET id='different-row' WHERE id=?1",
            [b.as_str()],
        )
        .unwrap();
    let before = snapshot(&store);
    let error = store.repair(&a, &b, None, now(), |_| Ok(())).unwrap_err();
    assert!(error.to_string().contains("keys disagree"));
    assert_eq!(snapshot(&store), before);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT id FROM agents WHERE id='different-row'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "different-row"
    );
}

#[test]
fn repair_refuses_conflicting_copies_capacity_and_unsupported_state() {
    for case in 0..7 {
        let store = Store::in_memory().unwrap();
        let (a, b) = seed(&store);
        match case {
            0 => {
                let first = envelope("same-id", &b);
                let mut second = first.clone();
                second.payload = json!({"text":"different"});
                store.enqueue(&a, &first, 1000).unwrap();
                store.enqueue(&b, &second, 1000).unwrap();
            }
            1 => {
                for i in 0..1001 {
                    let id = if i % 2 == 0 { &a } else { &b };
                    store
                        .enqueue(id, &envelope(&i.to_string(), id), 1000)
                        .unwrap();
                }
            }
            2 => {
                store
                    .put_document("future_kind", "item", &json!({"owner":b}))
                    .unwrap();
            }
            3 => {
                store
                    .put_document("access", "grant", &json!({"agent":a}))
                    .unwrap();
            }
            4 => {
                store
                    .put_document("restore_point", b.as_str(), &json!({}))
                    .unwrap();
            }
            5 => {
                let q = agentdocker_core::Question {
                    presentation: None,
                    id: "q".to_owned().into(),
                    from: b.to_string(),
                    to: Destination::Agent(a.clone()),
                    text: "review".into(),
                    asked_at: now(),
                    expires_at: now() + Duration::days(1),
                };
                store.put_document("question", "q", &q).unwrap();
            }
            _ => {
                store
                    .put_document("future_kind", b.as_str(), &json!({}))
                    .unwrap();
            }
        }
        let before = snapshot(&store);
        assert!(
            store.repair(&a, &b, None, now(), |_| Ok(())).is_err(),
            "case {case}"
        );
        assert_eq!(snapshot(&store), before, "case {case}");
    }
}

#[test]
fn wal_exclusive_maintenance_refuses_idle_connections_and_blocks_new_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let (a, b) = seed(&store);
    let preview_store = Store::open_repair(&path, false).unwrap();
    let plan = preview(&preview_store, &a, &b);
    drop(preview_store);
    assert!(
        Store::open_repair(&path, true).is_err(),
        "idle daemon connection must block repair"
    );
    drop(store);
    let maintenance = Store::open_repair(&path, true).unwrap();
    let contender = Connection::open(&path).unwrap();
    contender.busy_timeout(std::time::Duration::ZERO).unwrap();
    assert!(
        contender
            .query_row("SELECT COUNT(*) FROM agents", [], |r| r.get::<_, i64>(0))
            .is_err(),
        "new daemon must not read while maintenance owns the file"
    );
    drop(contender);
    maintenance
        .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| Ok(()))
        .unwrap();
    drop(maintenance);
    let restored = Store::open(&path).unwrap();
    assert_eq!(restored.load_agents().unwrap().len(), 1);
    let daemon =
        crate::Daemon::with_store(tmp.path().into(), tmp.path().join("custom.sock"), restored)
            .unwrap();
    assert_eq!(daemon.resolve(b.as_str()).unwrap(), a);
}

#[test]
fn preview_does_not_change_permissions_contents_or_schema() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let (a, b) = seed(&store);
    drop(store);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let store = Store::open_repair(&path, false).unwrap();
    preview(&store, &a, &b);
    drop(store);
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn invalid_alias_aborts_before_recovery_and_removal_cleans_valid_routes() {
    let store = Store::in_memory().unwrap();
    let (a, b) = seed(&store);
    let plan = preview(&store, &a, &b);
    store
        .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| Ok(()))
        .unwrap();
    store
        .delete_agent(
            &a,
            &Event::new(EventKind::AgentRemoved { agent: a.clone() }, now()),
        )
        .unwrap();
    assert!(store.identity_aliases().unwrap().is_empty());
    assert!(
        store
            .document::<Value>("identity_reconciliation", b.as_str())
            .unwrap()
            .is_some()
    );
    // Corrupt aliases must be rejected before recovering a not-yet-spawned job.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let mut managed = record("managed");
    managed.managed = true;
    managed.status = AgentStatus::Created;
    store.upsert_agent(&managed).unwrap();
    store
        .put_document(
            "identity_alias",
            "wrong-key",
            &AgentAlias {
                retired: b,
                canonical: a,
                retired_name: None,
                reconciled_at: now(),
            },
        )
        .unwrap();
    let before = snapshot(&store);
    let err = crate::Daemon::with_store(tmp.path().into(), tmp.path().join("socket"), store)
        .err()
        .unwrap();
    assert!(err.to_string().contains("key disagrees"));
    let store = Store::open(&path).unwrap();
    assert_eq!(snapshot(&store), before);
}

#[test]
fn removal_rolls_back_routes_queue_cursors_and_history_at_every_failure() {
    for table in ["documents", "inbox", "journal_cursors", "agents", "events"] {
        let store = Store::in_memory().unwrap();
        let (a, b) = seed(&store);
        store.enqueue(&b, &envelope("retained", &b), 1000).unwrap();
        let project = record("canonical").project.unwrap().id();
        store
            .set_journal_cursor(a.as_str(), &project, 7, now())
            .unwrap();
        store
            .set_journal_cursor(b.as_str(), &project, 7, now())
            .unwrap();
        let plan = preview(&store, &a, &b);
        store
            .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| Ok(()))
            .unwrap();
        let before = snapshot(&store);
        let mut event = Event::new(EventKind::AgentRemoved { agent: a.clone() }, now());
        event.seq = store.max_event_seq().unwrap() + 1;
        let operation = if table == "events" {
            "INSERT"
        } else {
            "DELETE"
        };
        store
            .conn
            .execute_batch(&format!(
                "CREATE TRIGGER reject_removal BEFORE {operation} ON {table}
             BEGIN SELECT RAISE(ABORT, 'fixture removal failure'); END;"
            ))
            .unwrap();
        assert!(store.delete_agent(&a, &event).is_err(), "{table}");
        assert_eq!(snapshot(&store), before, "{table}");
        assert_eq!(store.journal_cursor(a.as_str(), &project).unwrap(), Some(7));
        assert_eq!(store.identity_aliases().unwrap()[0].canonical, a);
        store
            .conn
            .execute_batch("DROP TRIGGER reject_removal")
            .unwrap();
        store.delete_agent(&a, &event).unwrap();
        assert!(store.load_agents().unwrap().is_empty());
        assert!(store.identity_aliases().unwrap().is_empty());
        assert!(store.load_inboxes().unwrap().is_empty());
        assert_eq!(store.journal_cursor(a.as_str(), &project).unwrap(), None);
        assert_eq!(store.recent_events(1).unwrap()[0].seq, event.seq);
        assert!(
            store
                .document::<Value>("identity_reconciliation", b.as_str())
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn repair_moves_typed_protection_and_membership_and_refuses_self_review() {
    let store = Store::in_memory().unwrap();
    let (a, b) = seed(&store);
    let project = record("canonical").project.unwrap().id();
    let mut channel = Channel {
        id: "room".into(),
        project,
        name: None,
        subject: agentdocker_core::ChannelSubject::Task {
            task: "retired".into(),
        },
        members: vec![a.clone(), b.clone()],
        opened_by: Some(b.clone()),
        opened_at: now(),
        reviews: vec![],
        closed_at: None,
        resolution: None,
    };
    store.put_document("channel", "room", &channel).unwrap();
    let mut card = agentdocker_core::Task {
        id: "repair-card".to_owned().into(),
        project: channel.project.clone(),
        title: "retired".into(),
        acceptance: "retired".into(),
        column: agentdocker_core::task::Column::Done,
        assignee: Some(b.clone()),
        created_by: b.to_string(),
        created_at: now(),
        updated_at: now(),
        archived_at: Some(now()),
    };
    store.put_document("task", card.id.as_str(), &card).unwrap();
    let lease = Lease {
        id: "protected".into(),
        holder: b.clone(),
        resource: ResourceKey::new("path:/fixture/checkout/file"),
        mode: LeaseMode::Exclusive,
        acquired_at: now(),
        expires_at: now() + Duration::days(1),
        change_seq: Some(8),
        note: Some("retired".into()),
        amount: 0,
    };
    store.upsert_lease(&lease).unwrap();
    let plan = preview(&store, &a, &b);
    store
        .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| Ok(()))
        .unwrap();
    let mut moved = lease.clone();
    moved.holder = a.clone();
    assert_eq!(store.load_leases().unwrap(), vec![moved]);
    channel.members = vec![a.clone()];
    channel.opened_by = Some(a.clone());
    assert_eq!(
        store.document::<Channel>("channel", "room").unwrap(),
        Some(channel)
    );
    let archive = store
        .document::<Value>("identity_reconciliation", b.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(
        archive["before"]["leases"][0],
        serde_json::to_value(lease).unwrap()
    );
    let original_card = serde_json::to_value(&card).unwrap();
    card.assignee = Some(a.clone());
    card.created_by = a.to_string();
    assert_eq!(
        store
            .document::<agentdocker_core::Task>("task", card.id.as_str())
            .unwrap(),
        Some(card)
    );
    assert!(
        archive["before"]["documents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|document| document["kind"] == "task" && document["value"] == original_card)
    );
    // New self-review must not silently become accepted review evidence.
    let doc = Document {
        kind: "channel".into(),
        id: "review".into(),
        value: json!({
            "id":"review","project":"project","subject":{"kind":"task","task":"review"},
            "members":[a,b],"opened_at":now(),"reviews":[{"by":b,"by_name":"old","of":a,"verdict":"approve","note":"review","at":now()}]
        }),
    };
    assert!(
        rewrite_document(&doc, &a, &b)
            .unwrap_err()
            .to_string()
            .contains("self-review")
    );
}

#[test]
fn missing_cursor_is_unread_and_incompatible_read_sets_are_refused() {
    let store = Store::in_memory().unwrap();
    let (a, b) = seed(&store);
    let project = record("canonical").project.unwrap().id();
    store
        .conn
        .execute(
            "INSERT INTO journal_cursors VALUES (?1,?2,9,?3)",
            params![b.as_str(), project.as_str(), now().to_rfc3339()],
        )
        .unwrap();
    let marks = |version: &str| {
        vec![ReadMark {
            path: "/fixture/file".into(),
            at: now(),
            version: version.into(),
            head: None,
        }]
    };
    store
        .put_document("reads", a.as_str(), &marks("one"))
        .unwrap();
    store
        .put_document("reads", b.as_str(), &marks("two"))
        .unwrap();
    assert!(
        store
            .repair(&a, &b, None, now(), |_| Ok(()))
            .unwrap_err()
            .to_string()
            .contains("read sets disagree")
    );
    store
        .put_document("reads", b.as_str(), &marks("one"))
        .unwrap();
    let plan = preview(&store, &a, &b);
    store
        .repair(&a, &b, Some(&plan.plan_sha256), now(), |_| Ok(()))
        .unwrap();
    assert_eq!(store.journal_cursor(a.as_str(), &project).unwrap(), Some(0));
}

/// A session come back folds every life it left into the one that ended
/// last: one queue with each message once, a question an earlier life
/// asked now the canonical record's, an alias that pointed at an earlier
/// life pointed at the canonical record, every retired id an alias — in
/// one transaction. A duplicate message that differs, a retired record
/// with conflicting observations or a lease of its own, or an id that is already an
/// alias refuses the plan and writes nothing.
#[test]
fn a_resumed_session_folds_every_life_it_left_once_and_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("state.db")).unwrap();
    let last = record("last");
    let earlier = record("earlier\0\'quoted");
    let fresh = record("fresh");
    let oldest = record("oldest");
    for r in [&last, &earlier, &fresh] {
        store.upsert_agent(r).unwrap();
    }
    // A life before any of these was retired into `earlier`.
    store
        .put_document(
            "identity_alias",
            oldest.id.as_str(),
            &AgentAlias {
                retired: oldest.id.clone(),
                canonical: earlier.id.clone(),
                retired_name: Some(oldest.spec.name.clone()),
                reconciled_at: now(),
            },
        )
        .unwrap();
    // The same broadcast reached both ended lives; each life has words of
    // its own too.
    // Clocks disagree: the fresh life's message says it was sent before
    // the earlier life's. The store's own order, not the senders' clocks,
    // is the order.
    let broadcast = envelope("b-1", &earlier.id);
    store.enqueue(&earlier.id, &broadcast, 64).unwrap();
    store.enqueue(&last.id, &broadcast, 64).unwrap();
    let mut e1 = envelope("e-1", &earlier.id);
    e1.sent_at = now() + Duration::minutes(5);
    store.enqueue(&earlier.id, &e1, 64).unwrap();
    let mut f1 = envelope("f-1", &fresh.id);
    f1.sent_at = now() - Duration::minutes(5);
    store.enqueue(&fresh.id, &f1, 64).unwrap();
    // A question the earlier life asked, still open.
    let question = agentdocker_core::Question {
        id: "q-1".to_owned().into(),
        from: earlier.id.to_string(),
        to: Destination::Agent("user".into()),
        text: "still?".into(),
        presentation: None,
        asked_at: now(),
        expires_at: now() + Duration::hours(1),
    };
    store.put_document("question", "q-1", &question).unwrap();
    let mut canonical = last.clone();
    canonical.pid = Some(456);
    canonical.status = AgentStatus::Running;
    canonical.finished_at = None;
    let retired = vec![fresh.id.clone(), earlier.id.clone()];
    for id in &retired {
        store
            .put_document("reads", id.as_str(), &Vec::<ReadMark>::new())
            .unwrap();
    }
    let before = snapshot(&store);
    let plan = store.plan_resume(&canonical, &retired).unwrap();
    assert_eq!(snapshot(&store), before, "planning writes nothing");
    assert_eq!(
        plan.queue
            .iter()
            .map(|m| m.id.to_string())
            .collect::<Vec<_>>(),
        ["b-1", "e-1", "f-1"],
        "the plan's queue is the store's order, each message once"
    );
    let mut event = Event::new(
        EventKind::SessionResumed {
            agent: last.id.clone(),
            retired: retired.clone(),
            session: "same-session".into(),
            pid: 456,
        },
        now(),
    );
    event.seq = 1;
    store.write_resume(&plan, &event).unwrap();
    let remaining = store.load_agents().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, last.id);
    assert_eq!(remaining[0].pid, Some(456));
    let queue: Vec<String> = store.load_inboxes().unwrap()[&last.id]
        .iter()
        .map(|m| m.id.to_string())
        .collect();
    assert_eq!(
        queue,
        ["b-1", "e-1", "f-1"],
        "each message once, in the plan's order"
    );
    let reopened = Store::open(&tmp.path().join("state.db")).unwrap();
    let queue: Vec<String> = reopened.load_inboxes().unwrap()[&last.id]
        .iter()
        .map(|m| m.id.to_string())
        .collect();
    assert_eq!(queue, ["b-1", "e-1", "f-1"], "and the same after a reopen");
    for id in &retired {
        assert!(
            reopened
                .document::<Vec<ReadMark>>("reads", id.as_str())
                .unwrap()
                .is_none(),
            "retired empty reads must not accumulate: {id}"
        );
    }
    drop(reopened);
    let rewritten: agentdocker_core::Question = store.document("question", "q-1").unwrap().unwrap();
    assert_eq!(
        rewritten.from,
        last.id.to_string(),
        "the canonical record's to cancel"
    );
    let aliases = store.identity_aliases().unwrap();
    for id in [&oldest.id, &earlier.id, &fresh.id] {
        assert_eq!(
            aliases
                .iter()
                .find(|a| &a.retired == id)
                .map(|a| &a.canonical),
            Some(&last.id),
            "{id} resolves to the record that stayed, flat"
        );
    }
    let mut registry = agentdocker_core::Registry::new();
    for r in &remaining {
        registry.insert(r.clone()).unwrap();
    }
    registry.restore_aliases(&aliases).unwrap();
    let mut names = vec![
        oldest.spec.name.clone(),
        earlier.spec.name.clone(),
        fresh.spec.name.clone(),
        last.spec.name.clone(),
    ];
    names.sort();
    names.dedup();
    assert_eq!(registry.identity_names(&last.id), names);
    assert!(store.recent_events(5).unwrap().iter().any(
        |e| matches!(&e.kind, EventKind::SessionResumed { retired, .. } if retired.len() == 2)
    ));

    // Refusals write nothing.
    let store = Store::open(&tmp.path().join("again.db")).unwrap();
    for r in [&last, &earlier, &fresh] {
        store.upsert_agent(r).unwrap();
    }
    let before = snapshot(&store);
    let mut other = envelope("b-1", &fresh.id);
    other.payload = json!({"text": "not the same words"});
    store.enqueue(&last.id, &broadcast, 64).unwrap();
    store.enqueue(&fresh.id, &other, 64).unwrap();
    let error = store.plan_resume(&canonical, &retired).unwrap_err();
    assert!(error.to_string().contains("different content"), "{error}");
    store
        .conn
        .execute("DELETE FROM inbox WHERE agent=?1", [fresh.id.as_str()])
        .unwrap();
    store
        .put_document(
            "reads",
            earlier.id.as_str(),
            &vec![agentdocker_core::ReadMark {
                path: "/fixture/checkout/src/lib.rs".into(),
                version: "v".into(),
                head: None,
                at: now(),
            }],
        )
        .unwrap();
    store
        .put_document(
            "reads",
            last.id.as_str(),
            &vec![ReadMark {
                path: "/fixture/checkout/src/lib.rs".into(),
                version: "different".into(),
                head: None,
                at: now(),
            }],
        )
        .unwrap();
    let error = store.plan_resume(&canonical, &retired).unwrap_err();
    assert!(error.to_string().contains("observations"), "{error}");
    store.delete_document("reads", earlier.id.as_str()).unwrap();
    store.delete_document("reads", last.id.as_str()).unwrap();
    store
        .upsert_lease(&agentdocker_core::Lease {
            id: agentdocker_core::LeaseId::generate(),
            resource: ResourceKey::new("task:x"),
            holder: fresh.id.clone(),
            mode: LeaseMode::Exclusive,
            acquired_at: now(),
            change_seq: None,
            expires_at: now() + Duration::hours(1),
            note: None,
            amount: 0,
        })
        .unwrap();
    let error = store.plan_resume(&canonical, &retired).unwrap_err();
    assert!(error.to_string().contains("lease"), "{error}");
    assert_eq!(
        snapshot(&store)[3],
        before[3],
        "no document was written by a refused plan"
    );
    assert!(
        store
            .plan_resume(&canonical, std::slice::from_ref(&last.id))
            .is_err()
    );
    // Queues that together exceed what one record may hold refuse too.
    store.conn.execute("DELETE FROM leases", []).unwrap();
    for n in 0..RESUME_QUEUE_MESSAGES {
        let to = if n % 2 == 0 { &earlier.id } else { &fresh.id };
        store
            .enqueue(to, &envelope(&format!("many-{n}"), to), 2000)
            .unwrap();
    }
    let error = store.plan_resume(&canonical, &retired).unwrap_err();
    assert!(error.to_string().contains("one record may hold"), "{error}");
    assert_eq!(snapshot(&store)[0], before[0], "nothing moved");
}

#[test]
fn resumed_read_sets_keep_latest_paths_and_roll_back_with_the_queue_and_event() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let (last, earlier, fresh) = (record("last"), record("earlier"), record("fresh"));
    for record in [&last, &earlier, &fresh] {
        store.upsert_agent(record).unwrap();
    }
    let mark = |path: &str, version: &str, seconds: i64| ReadMark {
        path: path.into(),
        version: version.into(),
        head: Some("head".into()),
        at: now() + Duration::seconds(seconds),
    };
    let old = mark("/checkout/shared", "old", 1);
    let latest = mark("/checkout/shared", "new", 3);
    let unique = mark("/checkout/earlier", "retained", 2);
    let fresh_mark = mark("/checkout/fresh", "fresh", 4);
    store
        .put_document("reads", last.id.as_str(), &vec![old])
        .unwrap();
    store
        .put_document(
            "reads",
            earlier.id.as_str(),
            &vec![unique.clone(), latest.clone()],
        )
        .unwrap();
    store
        .put_document(
            "reads",
            fresh.id.as_str(),
            &vec![fresh_mark.clone(), latest.clone()],
        )
        .unwrap();
    let message = envelope("retained-input", &earlier.id);
    store.enqueue(&earlier.id, &message, 64).unwrap();
    let retired = vec![fresh.id.clone(), earlier.id.clone()];
    let mut canonical = last.clone();
    canonical.pid = Some(456);
    canonical.status = AgentStatus::Running;
    canonical.finished_at = None;
    let mut event = Event::new(
        EventKind::SessionResumed {
            agent: last.id.clone(),
            retired: retired.clone(),
            session: "session".into(),
            pid: 456,
        },
        now(),
    );
    event.seq = 1;
    let mut room = Channel {
        id: "room".into(),
        project: last.project.as_ref().unwrap().id(),
        name: Some("room".into()),
        subject: agentdocker_core::channel::ChannelSubject::Task {
            task: "retained room".into(),
        },
        members: vec![
            earlier.id.clone(),
            last.id.clone(),
            fresh.id.clone(),
            "peer".into(),
        ],
        opened_by: Some(earlier.id.clone()),
        opened_at: now(),
        reviews: vec![],
        closed_at: None,
        resolution: None,
    };
    store
        .put_document("channel", room.id.as_str(), &room)
        .unwrap();
    // Even an eligible open room must refuse a fold that changes who reviewed whom.
    room.reviews.push(agentdocker_core::channel::Review {
        by: earlier.id.clone(),
        by_name: "earlier".into(),
        of: fresh.id.clone(),
        of_name: "fresh".into(),
        verdict: agentdocker_core::channel::Verdict::Approve,
        note: "review".into(),
        at: now(),
        head: None,
    });
    store
        .put_document("channel", room.id.as_str(), &room)
        .unwrap();
    let conflict = snapshot(&store);
    assert!(
        store
            .plan_resume(&canonical, &retired)
            .unwrap_err()
            .to_string()
            .contains("self-review")
    );
    assert_eq!(snapshot(&store), conflict);
    room.reviews.clear();
    store
        .put_document("channel", room.id.as_str(), &room)
        .unwrap();
    let before = snapshot(&store);
    let plan = store.plan_resume(&canonical, &retired).unwrap();
    assert_eq!(snapshot(&store), before);
    store.conn.execute_batch("CREATE TRIGGER refuse_resume_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").unwrap();
    assert!(store.write_resume(&plan, &event).is_err());
    assert_eq!(
        snapshot(&store),
        before,
        "the read rewrite, aliases and queue roll back together"
    );
    store
        .conn
        .execute_batch("DROP TRIGGER refuse_resume_event")
        .unwrap();
    store.write_resume(&plan, &event).unwrap();
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened
            .document::<Vec<ReadMark>>("reads", last.id.as_str())
            .unwrap(),
        Some(vec![unique, fresh_mark, latest])
    );
    for id in &retired {
        assert!(
            reopened
                .document::<Vec<ReadMark>>("reads", id.as_str())
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        reopened.load_inboxes().unwrap()[&last.id]
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>(),
        vec![message.id]
    );
    assert_eq!(reopened.identity_aliases().unwrap().len(), 2);
    let restored = reopened
        .document::<Channel>("channel", room.id.as_str())
        .unwrap()
        .unwrap();
    assert!(restored.is_open());
    assert_eq!(restored.members, vec![last.id.clone(), "peer".into()]);
    assert_eq!(restored.opened_by, Some(last.id));
}

#[test]
fn resumed_cards_keep_their_work_without_inventing_a_hold() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let (last, earlier, fresh) = (record("last"), record("earlier"), record("fresh"));
    for record in [&last, &earlier, &fresh] {
        store.upsert_agent(record).unwrap();
    }
    let task = agentdocker_core::Task {
        id: "resume-card".to_owned().into(),
        project: last.project.as_ref().unwrap().id(),
        title: earlier.id.to_string(),
        acceptance: fresh.id.to_string(),
        column: agentdocker_core::task::Column::InProgress,
        assignee: Some(earlier.id.clone()),
        created_by: fresh.id.to_string(),
        created_at: now(),
        updated_at: now(),
        archived_at: None,
    };
    store.put_document("task", task.id.as_str(), &task).unwrap();
    let lease = Lease {
        id: "card-hold".into(),
        holder: earlier.id.clone(),
        resource: ResourceKey::new("task:resume-card"),
        mode: LeaseMode::Exclusive,
        acquired_at: now(),
        expires_at: now() + Duration::days(1),
        change_seq: None,
        note: None,
        amount: 0,
    };
    store.upsert_lease(&lease).unwrap();
    let retired = vec![fresh.id.clone(), earlier.id.clone()];
    let held = snapshot(&store);
    assert!(
        store
            .plan_resume(&last, &retired)
            .unwrap_err()
            .to_string()
            .contains("holds a lease")
    );
    assert_eq!(snapshot(&store), held);
    store.delete_lease(&lease.id).unwrap();
    let message = envelope("resume-card-input", &earlier.id);
    store.enqueue(&earlier.id, &message, 64).unwrap();
    let mut event = Event::new(
        EventKind::SessionResumed {
            agent: last.id.clone(),
            retired: retired.clone(),
            session: "same-session".into(),
            pid: 456,
        },
        now(),
    );
    event.seq = 1;
    let before = snapshot(&store);
    let plan = store.plan_resume(&last, &retired).unwrap();
    assert_eq!(snapshot(&store), before);
    store.conn.execute_batch("CREATE TRIGGER refuse_card_resume BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").unwrap();
    assert!(store.write_resume(&plan, &event).is_err());
    assert_eq!(
        snapshot(&store),
        before,
        "card, queue and aliases roll back together"
    );
    store
        .conn
        .execute_batch("DROP TRIGGER refuse_card_resume")
        .unwrap();
    store.write_resume(&plan, &event).unwrap();
    drop(store);
    let reopened = Store::open(&path).unwrap();
    let mut expected = task;
    expected.assignee = Some(last.id.clone());
    expected.created_by = last.id.to_string();
    let actual = reopened
        .document::<agentdocker_core::Task>("task", expected.id.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(
        actual, expected,
        "free text, column and dates are preserved exactly"
    );
    assert!(
        reopened.load_leases().unwrap().is_empty(),
        "resumption creates no work hold"
    );
    assert_eq!(reopened.load_inboxes().unwrap()[&last.id][0].id, message.id);
    assert_eq!(reopened.identity_aliases().unwrap().len(), 2);
    assert_eq!(
        reopened
            .tasks_page(None, None, false, 0, 10, 65536)
            .unwrap()
            .0,
        vec![expected]
    );
}

#[test]
fn conflicting_or_excessive_resumed_observations_leave_every_record_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("state.db")).unwrap();
    let (last, earlier, fresh) = (record("last"), record("earlier"), record("fresh"));
    for record in [&last, &earlier, &fresh] {
        store.upsert_agent(record).unwrap();
    }
    let retired = vec![fresh.id.clone(), earlier.id.clone()];
    let mark = |path: String, at, version: &str| ReadMark {
        path: path.into(),
        at,
        version: version.into(),
        head: None,
    };
    // Conflicting older captures must fail even when a newer one was visited first.
    store
        .put_document(
            "reads",
            last.id.as_str(),
            &vec![mark("/same".into(), now() + Duration::seconds(1), "newest")],
        )
        .unwrap();
    store
        .put_document(
            "reads",
            earlier.id.as_str(),
            &vec![mark("/same".into(), now(), "one")],
        )
        .unwrap();
    store
        .put_document(
            "reads",
            fresh.id.as_str(),
            &vec![mark("/same".into(), now(), "two")],
        )
        .unwrap();
    let before = snapshot(&store);
    for ids in [retired.clone(), retired.iter().rev().cloned().collect()] {
        assert!(
            store
                .plan_resume(&last, &ids)
                .unwrap_err()
                .to_string()
                .contains("same capture time")
        );
        assert_eq!(snapshot(&store), before);
    }
    store
        .put_document(
            "reads",
            earlier.id.as_str(),
            &vec![mark("/same".into(), now(), "two")],
        )
        .unwrap();
    let marks: Vec<_> = (0..RESUME_READS)
        .map(|i| mark(format!("/unique-{i}"), now(), "v"))
        .collect();
    store
        .put_document("reads", fresh.id.as_str(), &marks)
        .unwrap();
    let before = snapshot(&store);
    assert!(
        store
            .plan_resume(&last, &retired)
            .unwrap_err()
            .to_string()
            .contains("capacity")
    );
    assert_eq!(snapshot(&store), before);
    store
        .put_document(
            "reads",
            fresh.id.as_str(),
            &vec![mark("/large".into(), now(), &"x".repeat(RESUME_READ_BYTES))],
        )
        .unwrap();
    let before = snapshot(&store);
    assert!(
        store
            .plan_resume(&last, &retired)
            .unwrap_err()
            .to_string()
            .contains("4 MiB")
    );
    assert_eq!(snapshot(&store), before);
}
