//! A bounded maintenance transaction. Routing state moves; history tables do not.
use super::*;
use agentdocker_core::identity::{AgentAlias, repair_pair};
use agentdocker_core::{Channel, Checkpoint, Contest, HandoffBundle, ReadMark, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const ROW_LIMIT: usize = 50_000;
const INPUT_BYTES: usize = 64 * 1024 * 1024;
const ARCHIVE_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepairPreview {
    pub canonical: AgentId,
    pub retired: AgentId,
    pub plan_sha256: String,
    pub moved: BTreeMap<String, usize>,
    pub applied: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Document {
    kind: String,
    id: String,
    value: Value,
}

pub(crate) struct RepairPlan {
    pub preview: RepairPreview,
    pub records: Vec<AgentRecord>,
    canonical: AgentRecord,
    aliases: Vec<AgentAlias>,
    leases: Vec<Lease>,
    documents: Vec<Document>,
    remove_documents: Vec<(String, String)>,
    duplicate_inbox_rows: Vec<i64>,
    cursors: BTreeMap<String, i64>,
    before: Value,
}

impl Store {
    pub(crate) fn identity_aliases(&self) -> Result<Vec<AgentAlias>> {
        let mut statement = self
            .conn
            .prepare("SELECT id,json FROM documents WHERE kind='identity_alias' ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (id, raw) = row?;
            let alias: AgentAlias = serde_json::from_str(&raw)?;
            anyhow::ensure!(
                id == alias.retired.as_str(),
                "identity alias key disagrees with its retired ID"
            );
            Ok(alias)
        })
        .collect()
    }

    fn check_repair_size(&self) -> Result<()> {
        let mut total_bytes = 0i64;
        for table in ["agents", "leases", "inbox", "documents"] {
            let (rows, bytes): (i64, i64) = self.conn.query_row(
                &format!(
                    "SELECT COUNT(*), COALESCE(SUM(length(CAST(json AS BLOB))),0) FROM {table}"
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            total_bytes = total_bytes.saturating_add(bytes);
            anyhow::ensure!(
                rows <= ROW_LIMIT as i64 && total_bytes <= INPUT_BYTES as i64,
                "state exceeds bounded identity-repair inspection capacity"
            );
        }
        let cursor_bytes: i64 = self.conn.query_row("SELECT COALESCE(SUM(length(agent)+length(project)+length(updated_at)+8),0) FROM journal_cursors", [], |row|row.get(0))?;
        let cursor_rows: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM journal_cursors", [], |row| row.get(0))?;
        anyhow::ensure!(
            cursor_rows <= ROW_LIMIT as i64
                && total_bytes.saturating_add(cursor_bytes) <= INPUT_BYTES as i64,
            "journal cursors exceed bounded identity-repair inspection capacity"
        );
        Ok(())
    }

    pub(crate) fn repair(
        &self,
        kept: &AgentId,
        retired: &AgentId,
        expected: Option<&str>,
        now: DateTime<Utc>,
        quiescent: impl Fn(&[AgentRecord]) -> Result<()>,
    ) -> Result<RepairPreview> {
        let tx = self.conn.unchecked_transaction()?;
        self.check_repair_size()?;
        // The JSON describes the identity evidence, while writes address SQL
        // keys. Refuse disagreement before either one can authorize a move of
        // a different stored record. This also applies to receipt replay.
        for (table, mismatch) in [
            (
                "agents",
                "id IS NOT json_extract(json,'$.id') OR name IS NOT json_extract(json,'$.spec.name')",
            ),
            (
                "leases",
                "id IS NOT json_extract(json,'$.id') OR holder IS NOT json_extract(json,'$.holder') OR resource IS NOT json_extract(json,'$.resource')",
            ),
        ] {
            let inconsistent: bool = self.conn.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {mismatch})"),
                [],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                !inconsistent,
                "{table} keys disagree with stored identity evidence; repair refused"
            );
        }
        if let Some(alias) = self.document::<AgentAlias>("identity_alias", retired.as_str())? {
            anyhow::ensure!(
                &alias.canonical == kept,
                "former ID already resolves to a different canonical record"
            );
            let archive = self
                .document::<Value>("identity_reconciliation", retired.as_str())?
                .context("identity archive is missing")?;
            let mut preview: RepairPreview = serde_json::from_value(archive["plan"].clone())?;
            anyhow::ensure!(
                archive["format"] == 1 && preview.canonical == *kept && preview.retired == *retired,
                "the original repair archive does not describe this pair; inspect the current canonical identity"
            );
            if let Some(expected) = expected {
                anyhow::ensure!(
                    preview.plan_sha256 == expected,
                    "repair plan has changed; inspect again"
                );
            }
            preview.applied = true;
            tx.rollback()?;
            return Ok(preview);
        }
        let plan = self.plan_repair(kept, retired)?;
        let mut preview = plan.preview.clone();
        if let Some(expected) = expected {
            anyhow::ensure!(
                expected == preview.plan_sha256,
                "repair plan has changed; inspect again"
            );
            quiescent(&plan.records)?;
            self.write_repair(&plan, now)?;
            tx.commit()?;
            preview.applied = true;
        } else {
            tx.rollback()?;
        }
        Ok(preview)
    }
    /// Inspection never initializes or upgrades state. Exclusive locking mode
    /// is selected before the first database read for apply. In WAL mode this
    /// refuses even an idle existing connection, independently of socket names.
    pub(crate) fn open_repair(path: &Path, exclusive: bool) -> Result<Self> {
        agentdocker_host::dirs::read_private_file(path)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut companion = path.as_os_str().to_owned();
            companion.push(suffix);
            let companion = PathBuf::from(companion);
            match std::fs::symlink_metadata(&companion) {
                Ok(_) => {
                    agentdocker_host::dirs::read_private_file(&companion)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        let flags = if exclusive {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        if exclusive {
            conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        }
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .context(
                "state is still open elsewhere; stop its daemon before applying identity repair",
            )?;
        anyhow::ensure!(
            mode.eq_ignore_ascii_case("wal"),
            "identity repair requires existing WAL state"
        );
        let schema: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            schema
                .parse::<i64>()
                .is_ok_and(|n| (10..=SCHEMA_VERSION).contains(&n)),
            "identity repair requires schema 10 through {SCHEMA_VERSION}; found {schema}"
        );
        Ok(Self { conn, fts: false })
    }

    /// Must run inside the caller's snapshot/maintenance transaction.
    pub(crate) fn plan_repair(&self, kept: &AgentId, retired: &AgentId) -> Result<RepairPlan> {
        let mut records = self.load_agents()?;
        records.sort_by(|a, b| a.id.cmp(&b.id));
        repair_pair(&records, kept, retired).map_err(anyhow::Error::msg)?;
        let old_record = records
            .iter()
            .find(|r| &r.id == retired)
            .expect("validated");
        let mut canonical = records
            .iter()
            .find(|r| &r.id == kept)
            .expect("validated")
            .clone();
        for (key, value) in &old_record.spec.labels {
            if canonical
                .spec
                .labels
                .get(key)
                .is_none_or(|current| current.is_empty())
            {
                canonical.spec.labels.insert(key.clone(), value.clone());
            }
        }
        canonical.last_seen = canonical.last_seen.max(old_record.last_seen);
        let mut moved = BTreeMap::new();
        let mut all_leases = self.load_leases()?;
        all_leases.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        let leases: Vec<_> = all_leases
            .iter()
            .filter(|l| &l.holder == retired)
            .cloned()
            .map(|mut l| {
                l.holder = kept.clone();
                l
            })
            .collect();
        for old in &leases {
            anyhow::ensure!(
                !all_leases
                    .iter()
                    .any(|l| &l.holder == kept && l.resource.overlaps(&old.resource)),
                "the identities hold overlapping leases; resolve their ownership before repair"
            );
        }
        moved.insert("leases".into(), leases.len());
        let mut stmt = self.conn.prepare(
            "SELECT seq, agent, message_id, json FROM inbox WHERE agent IN (?1, ?2) ORDER BY seq",
        )?;
        let inbox: Vec<(i64, String, String, String)> = stmt
            .query_map(params![kept.as_str(), retired.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut seen = BTreeMap::new();
        let mut bytes = 0usize;
        let mut duplicate_inbox_rows = Vec::new();
        for (seq, _, id, raw) in &inbox {
            let envelope: Envelope = serde_json::from_str(raw)?;
            anyhow::ensure!(
                envelope.id.as_str() == id,
                "stored inbox message ID disagrees with its envelope"
            );
            if let Some(previous) = seen.get(id) {
                anyhow::ensure!(
                    previous == &envelope,
                    "duplicate message ID carries different content"
                );
                duplicate_inbox_rows.push(*seq);
            } else {
                bytes = bytes.saturating_add(serde_json::to_vec(&envelope)?.len());
                seen.insert(id.clone(), envelope);
            }
        }
        anyhow::ensure!(
            seen.len() <= 1000 && bytes <= 4 * 1024 * 1024,
            "combined inbox exceeds admission capacity; acknowledge messages before repair"
        );
        moved.insert(
            "inbox".into(),
            inbox
                .iter()
                .filter(|(_, agent, _, _)| agent == retired.as_str())
                .count(),
        );
        moved.insert("duplicate_inbox_copies".into(), duplicate_inbox_rows.len());
        let mut stmt = self
            .conn
            .prepare("SELECT kind,id,json FROM documents ORDER BY kind,id")?;
        let raw_docs: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut docs = Vec::new();
        for (kind, id, raw) in &raw_docs {
            docs.push(Document {
                kind: kind.clone(),
                id: id.clone(),
                value: serde_json::from_str(raw)?,
            });
        }
        let mut aliases = self.identity_aliases()?;
        let mut registry = agentdocker_core::Registry::new();
        for r in &records {
            registry.insert(r.clone())?;
        }
        registry.restore_aliases(&aliases)?;
        for alias in &mut aliases {
            if &alias.canonical == retired {
                alias.canonical = kept.clone();
            }
        }
        let mut documents = Vec::new();
        let mut old_documents = Vec::new();
        let mut remove_documents = Vec::new();
        for doc in &docs {
            if doc.kind == "identity_alias" && doc.value["canonical"] == retired.as_str() {
                old_documents.push(doc.clone());
            }
            if doc.kind == "reads" && (doc.id == retired.as_str() || doc.id == kept.as_str()) {
                continue;
            }
            let value = rewrite_document(doc, kept, retired)?;
            if value != doc.value {
                *moved.entry(doc.kind.clone()).or_insert(0) += 1;
                old_documents.push(doc.clone());
                documents.push(Document {
                    value,
                    ..doc.clone()
                });
            }
        }
        let read_docs: Vec<_> = docs
            .iter()
            .filter(|d| d.kind == "reads" && (d.id == kept.as_str() || d.id == retired.as_str()))
            .collect();
        if read_docs.iter().any(|d| d.id == retired.as_str()) {
            let mut reads = BTreeMap::<PathBuf, ReadMark>::new();
            for doc in read_docs {
                old_documents.push(doc.clone());
                for mark in serde_json::from_value::<Vec<ReadMark>>(doc.value.clone())? {
                    if let Some(before) = reads.get(&mark.path) {
                        anyhow::ensure!(
                            before.version == mark.version && before.head == mark.head,
                            "read sets disagree about the same physical path"
                        );
                        if before.at > mark.at {
                            continue;
                        }
                    }
                    reads.insert(mark.path.clone(), mark);
                }
            }
            anyhow::ensure!(reads.len() <= 1000, "combined read set exceeds capacity");
            moved.insert("reads".into(), reads.len());
            documents.push(Document {
                kind: "reads".into(),
                id: kept.to_string(),
                value: serde_json::to_value(reads.into_values().collect::<Vec<_>>())?,
            });
            remove_documents.push(("reads".into(), retired.to_string()));
        }
        let mut stmt=self.conn.prepare("SELECT agent,project,seq,updated_at FROM journal_cursors WHERE agent IN (?1,?2) ORDER BY agent,project")?;
        let old_cursors: Vec<(String, String, i64, String)> = stmt
            .query_map(params![kept.as_str(), retired.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut cursor_pairs = BTreeMap::<String, (Option<i64>, Option<i64>)>::new();
        for (agent, project, seq, _) in &old_cursors {
            anyhow::ensure!(*seq >= 0, "negative journal cursor");
            let pair = cursor_pairs.entry(project.clone()).or_default();
            if agent == kept.as_str() {
                pair.0 = Some(*seq)
            } else {
                pair.1 = Some(*seq)
            }
        }
        // The least-read cursor preserves unread history. MAX would silently
        // mark one identity's unread work as already consumed by the other.
        let cursors = cursor_pairs
            .into_iter()
            .map(|(project, (a, b))| (project, a.unwrap_or(0).min(b.unwrap_or(0))))
            .collect::<BTreeMap<_, _>>();
        moved.insert("journal_cursors".into(), cursors.len());
        let before = serde_json::json!({"canonical":records.iter().find(|r| &r.id==kept),"retired":old_record,"documents":old_documents,"leases":all_leases.iter().filter(|l| &l.holder==retired).collect::<Vec<_>>(),"inbox":inbox,"cursors":old_cursors});
        anyhow::ensure!(
            serde_json::to_vec(&before)?.len() <= ARCHIVE_BYTES,
            "repair before-images exceed archive capacity"
        );
        let fingerprint = serde_json::to_vec(
            &serde_json::json!({"format":1,"schema":SCHEMA_VERSION,"effects":{"canonical":canonical,"aliases":aliases,"leases":leases,"documents":documents,"remove_documents":remove_documents,"duplicate_inbox_rows":duplicate_inbox_rows,"cursors":cursors},"records":records,"leases":all_leases,"documents":raw_docs,"inbox":inbox,"cursors":old_cursors,"event_seq":self.max_event_seq()?,"canonical":kept,"retired":retired}),
        )?;
        let preview = RepairPreview {
            canonical: kept.clone(),
            retired: retired.clone(),
            plan_sha256: format!("{:x}", Sha256::digest(fingerprint)),
            moved,
            applied: false,
        };
        Ok(RepairPlan {
            preview,
            records,
            canonical,
            aliases,
            leases,
            documents,
            remove_documents,
            duplicate_inbox_rows,
            cursors,
            before,
        })
    }

    /// Caller owns the exclusive database connection and a single transaction.
    /// Event failure therefore rolls every move, alias and schema change back.
    pub(crate) fn write_repair(&self, plan: &RepairPlan, now: DateTime<Utc>) -> Result<()> {
        let kept = &plan.preview.canonical;
        let retired = &plan.preview.retired;
        for lease in &plan.leases {
            self.upsert_lease(lease)?;
        }
        for seq in &plan.duplicate_inbox_rows {
            self.conn.execute("DELETE FROM inbox WHERE seq=?1", [seq])?;
        }
        self.conn.execute(
            "UPDATE inbox SET agent=?1 WHERE agent=?2",
            params![kept.as_str(), retired.as_str()],
        )?;
        for doc in &plan.documents {
            self.put_document(&doc.kind, &doc.id, &doc.value)?;
        }
        for (kind, id) in &plan.remove_documents {
            self.delete_document(kind, id)?;
        }
        for (project, seq) in &plan.cursors {
            self.conn.execute("INSERT INTO journal_cursors(agent,project,seq,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(agent,project) DO UPDATE SET seq=excluded.seq,updated_at=excluded.updated_at",params![kept.as_str(),project,seq,now.to_rfc3339()])?;
        }
        self.conn.execute(
            "DELETE FROM journal_cursors WHERE agent=?1",
            [retired.as_str()],
        )?;
        self.upsert_agent(&plan.canonical)?;
        self.conn
            .execute("DELETE FROM agents WHERE id=?1", [retired.as_str()])?;
        for alias in &plan.aliases {
            self.put_document("identity_alias", alias.retired.as_str(), alias)?;
        }
        let alias = AgentAlias {
            retired: retired.clone(),
            canonical: kept.clone(),
            reconciled_at: now,
        };
        self.put_document("identity_alias", retired.as_str(), &alias)?;
        self.put_document(
            "identity_reconciliation",
            retired.as_str(),
            &serde_json::json!({"format":1,"alias":alias,"plan":plan.preview,"before":plan.before}),
        )?;
        self.conn.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            [SCHEMA_VERSION.to_string()],
        )?;
        let mut event = Event::new(
            agentdocker_core::EventKind::AgentReconciled {
                canonical: kept.clone(),
                retired: retired.clone(),
                plan_sha256: plan.preview.plan_sha256.clone(),
            },
            now,
        );
        event.seq = self
            .max_event_seq()?
            .checked_add(1)
            .context("event sequence exhausted")?;
        self.append_event(&event)?;
        Ok(())
    }
}

fn replace(id: &mut AgentId, kept: &AgentId, old: &AgentId) {
    if id == old {
        *id = kept.clone();
    }
}
fn optional(id: &mut Option<AgentId>, kept: &AgentId, old: &AgentId) {
    if let Some(id) = id {
        replace(id, kept, old);
    }
}
fn unique(ids: &mut Vec<AgentId>) {
    let mut seen = BTreeSet::new();
    ids.retain(|id| seen.insert(id.clone()));
}
fn contains(value: &Value, id: &AgentId) -> bool {
    match value {
        Value::String(s) => s == id.as_str(),
        Value::Array(a) => a.iter().any(|v| contains(v, id)),
        Value::Object(o) => o.values().any(|v| contains(v, id)),
        _ => false,
    }
}

fn rewrite_document(doc: &Document, kept: &AgentId, old: &AgentId) -> Result<Value> {
    if doc.kind == "access"
        && (doc.value.get("agent") == Some(&Value::String(kept.to_string()))
            || doc.value.get("agent") == Some(&Value::String(old.to_string())))
    {
        anyhow::bail!("scoped access grants must be resolved before identity repair");
    }
    // Historical archives are deliberately immutable. Current aliases are
    // retargeted as typed records by write_repair, never by recursive text edits.
    if matches!(
        doc.kind.as_str(),
        "identity_reconciliation" | "identity_alias"
    ) {
        return Ok(doc.value.clone());
    }
    if doc.kind == "restore_point" && (doc.id == old.as_str() || doc.id == kept.as_str()) {
        anyhow::bail!("restore intent must be resolved before identity repair");
    }
    if doc.id != old.as_str() && !contains(&doc.value, old) {
        return Ok(doc.value.clone());
    }
    Ok(match doc.kind.as_str() {
        "channel" => {
            let mut c: Channel = serde_json::from_value(doc.value.clone())?;
            for id in &mut c.members {
                replace(id, kept, old);
            }
            unique(&mut c.members);
            optional(&mut c.opened_by, kept, old);
            for review in &mut c.reviews {
                let distinct = review.by != review.of;
                replace(&mut review.by, kept, old);
                replace(&mut review.of, kept, old);
                anyhow::ensure!(
                    !distinct || review.by != review.of,
                    "repair would turn an existing review into a self-review"
                );
            }
            serde_json::to_value(c)?
        }
        "question" => {
            let mut q: agentdocker_core::Question = serde_json::from_value(doc.value.clone())?;
            let was_self =
                matches!(&q.to,agentdocker_core::Destination::Agent(to) if to.as_str()==q.from);
            if q.from == old.as_str() {
                q.from = kept.to_string();
            }
            if let agentdocker_core::Destination::Agent(to) = &mut q.to {
                replace(to, kept, old);
            }
            let is_self =
                matches!(&q.to,agentdocker_core::Destination::Agent(to) if to.as_str()==q.from);
            anyhow::ensure!(
                was_self || !is_self,
                "repair would turn a pending question into a self-question"
            );
            serde_json::to_value(q)?
        }
        "checkpoint" => {
            let mut c: Checkpoint = serde_json::from_value(doc.value.clone())?;
            replace(&mut c.from, kept, old);
            optional(&mut c.accepted_by, kept, old);
            serde_json::to_value(c)?
        }
        "validation" => {
            let mut v: Validation = serde_json::from_value(doc.value.clone())?;
            replace(&mut v.agent, kept, old);
            if let Some(c) = &mut v.container {
                anyhow::ensure!(
                    &c.agent != old,
                    "container validation identity requires a separate transfer"
                );
            }
            serde_json::to_value(v)?
        }
        "handoff" => {
            let mut h: HandoffBundle = serde_json::from_value(doc.value.clone())?;
            let distinct = h.to.as_ref().is_some_and(|to| to != &h.from);
            replace(&mut h.from, kept, old);
            optional(&mut h.to, kept, old);
            anyhow::ensure!(
                !distinct || h.to.as_ref() != Some(&h.from),
                "repair would turn a handoff into a self-handoff"
            );
            for l in &mut h.leases {
                replace(&mut l.holder, kept, old);
            }
            serde_json::to_value(h)?
        }
        "contest" => {
            let mut c: Contest = serde_json::from_value(doc.value.clone())?;
            if c.opened_by == old.as_str() {
                c.opened_by = kept.to_string();
            }
            for id in &mut c.entrants {
                replace(id, kept, old);
            }
            unique(&mut c.entrants);
            let mut entries = BTreeSet::new();
            for entry in &mut c.entries {
                replace(&mut entry.agent, kept, old);
                anyhow::ensure!(
                    entries.insert(entry.agent.clone()),
                    "both identities have contest entries; resolve the contest before repair"
                );
            }
            optional(&mut c.winner, kept, old);
            serde_json::to_value(c)?
        }
        "reads" => doc.value.clone(),
        _ => anyhow::bail!(
            "document kind {} contains the retired identity and requires explicit migration support",
            doc.kind
        ),
    })
}
