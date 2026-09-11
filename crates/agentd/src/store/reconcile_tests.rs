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
