//! Codex 0.154 persists Pre/PostToolUse context as a tagged developer message,
//! omitted by thread/items/list. Scan the fixed suffix after our offer without
//! retaining arbitrary transcript bodies (compaction records can be very large).
use super::{
    super::ledger::Receipt,
    ledger::{Binding, HookOffer},
};
use agentdocker_host::{dirs, files};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const MAX_SUFFIX: u64 = 4 * 1024 * 1024;
const MAX_CONTEXT: usize = 6000;
const MAX_KEY_BYTES: usize = 256;
const MAX_OBJECT_KEYS: usize = 256;
const MAX_DEPTH: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    path: PathBuf,
    device: u64,
    inode: u64,
    offset: u64,
}

fn open(path: &Path, binding: &Binding) -> Result<File> {
    let root = Path::new(&binding.provider.profile)
        .join("sessions")
        .canonicalize()?;
    ensure!(
        path.is_absolute() && path.canonicalize()? == path && path.starts_with(&root),
        "hook transcript is outside the bound profile"
    );
    let mut file = dirs::read_private_file(path)?;
    let mut first = Vec::new();
    BufReader::new((&mut file).take(64 * 1024)).read_until(b'\n', &mut first)?;
    ensure!(
        first.last() == Some(&b'\n'),
        "hook transcript metadata exceeds bound"
    );
    let record: Value = serde_json::from_slice(&first)?;
    ensure!(
        record["type"] == "session_meta"
            && record["payload"]["id"].as_str() == Some(&binding.provider.session)
            && record["payload"]["cwd"]
                .as_str()
                .and_then(|v| Path::new(v).canonicalize().ok())
                .as_ref()
                == Some(&binding.cwd),
        "hook transcript metadata differs from the bound conversation"
    );
    Ok(file)
}

impl Snapshot {
    pub fn capture(path: &Path, binding: &Binding) -> Result<Self> {
        let path = path.canonicalize()?;
        let mut file = open(&path, binding)?;
        let meta = file.metadata()?;
        let identity = files::identity(&file)?;
        let end = meta.len();
        let start = end.saturating_sub(MAX_SUFFIX);
        file.seek(SeekFrom::Start(start))?;
        let mut suffix = Vec::new();
        file.take(end - start).read_to_end(&mut suffix)?;
        // Start at the last complete boundary if the provider is appending a
        // partial record now. No offered context can precede this snapshot.
        let offset = suffix
            .iter()
            .rposition(|b| *b == b'\n')
            .map(|i| start + i as u64 + 1)
            .context("hook transcript has no bounded complete record boundary")?;
        Ok(Self {
            path,
            device: identity.device,
            inode: identity.inode,
            offset,
        })
    }

    pub fn find(&self, binding: &Binding, context: &str) -> Result<Option<Receipt>> {
        let mut file = open(&self.path, binding)?;
        let meta = file.metadata()?;
        let identity = files::identity(&file)?;
        ensure!(
            identity.device == self.device
                && identity.inode == self.inode
                && meta.len() >= self.offset,
            "hook transcript was replaced or truncated"
        );
        let length = meta.len() - self.offset;
        ensure!(
            context.len() <= MAX_CONTEXT,
            "hook context exceeds its bound"
        );
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file.take(length));
        let mut found = None;
        while !reader.fill_buf()?.is_empty() {
            let mut record = RecordReader {
                reader: &mut reader,
            };
            let value = record.value(Shape::Record, 0).and_then(|value| {
                record.space()?;
                ensure!(record.peek()?.is_none(), "trailing hook transcript data");
                Ok(value)
            });
            // A partial last record is not evidence, even if its prefix is
            // malformed. Finish drains without buffering, including on error.
            if !record.finish()? {
                break;
            }
            let value = value.context("invalid hook transcript record")?;
            if let Some(receipt) = receipt(&value, binding, context)? {
                ensure!(
                    found.is_none(),
                    "multiple transcript receipts match one hook offer"
                );
                found = Some(receipt);
            }
        }
        let again = open(&self.path, binding)?;
        let current = again.metadata()?;
        let identity = files::identity(&again)?;
        ensure!(
            current.is_file()
                && identity.device == self.device
                && identity.inode == self.inode
                && current.len() >= meta.len(),
            "hook transcript changed while reading"
        );
        Ok(found)
    }
}

/// Only these fields can prove a receipt. All other JSON is still validated,
/// including duplicate keys, but its strings and arrays are not retained.
#[derive(Clone, Copy)]
enum Shape {
    Ignore,
    Record,
    Payload,
    Metadata,
    Content,
    ContentItem,
    Kinds,
    Text(usize),
}

impl Shape {
    fn field(self, key: &str) -> Self {
        match (self, key) {
            (Self::Record, "type")
            | (Self::Payload, "type" | "role")
            | (Self::ContentItem, "type") => Self::Text(128),
            (Self::Record, "payload") => Self::Payload,
            (Self::Payload, "id") | (Self::Metadata, "turn_id") => Self::Text(128),
            (Self::Payload, "content") => Self::Content,
            (Self::Payload, "internal_chat_message_metadata_passthrough") => Self::Metadata,
            (Self::ContentItem, "text") => Self::Text(MAX_CONTEXT),
            (Self::Metadata, "content_item_kinds") => Self::Kinds,
            _ => Self::Ignore,
        }
    }
}

/// A JSONL record ends at the next newline, not at an arbitrary read boundary.
/// Limits apply to retained keys, nesting and receipt fields; record bytes are
/// streamed. A complete scan is bounded by the file length captured in find(),
/// not by a latency promise or a maximum lifetime transcript growth.
struct RecordReader<'a, R> {
    reader: &'a mut R,
}

impl<R: BufRead> RecordReader<'_, R> {
    fn peek(&mut self) -> Result<Option<u8>> {
        Ok(self
            .reader
            .fill_buf()?
            .first()
            .copied()
            .filter(|b| *b != b'\n'))
    }

    fn byte(&mut self) -> Result<u8> {
        let byte = self.peek()?.context("incomplete JSON record")?;
        self.reader.consume(1);
        Ok(byte)
    }

    fn expect(&mut self, expected: u8) -> Result<()> {
        ensure!(self.byte()? == expected, "unexpected JSON byte");
        Ok(())
    }

    fn space(&mut self) -> Result<()> {
        while matches!(self.peek()?, Some(b' ' | b'\t' | b'\r')) {
            self.byte()?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<bool> {
        loop {
            let bytes = self.reader.fill_buf()?;
            if bytes.is_empty() {
                return Ok(false);
            }
            let end = bytes.iter().position(|b| *b == b'\n');
            let used = end.map_or(bytes.len(), |at| at + 1);
            self.reader.consume(used);
            if end.is_some() {
                return Ok(true);
            }
        }
    }

    fn value(&mut self, shape: Shape, depth: usize) -> Result<Value> {
        ensure!(depth < MAX_DEPTH, "JSON nesting exceeds its bound");
        self.space()?;
        match self.peek()?.context("missing JSON value")? {
            b'{' => self.object(shape, depth + 1),
            b'[' => self.array(shape, depth + 1),
            b'"' => {
                let limit = match shape {
                    Shape::Text(limit) => limit,
                    _ => 0,
                };
                Ok(self.string(limit)?.map_or(Value::Null, Value::String))
            }
            b't' | b'f' | b'n' => {
                let expected: &[u8] = match self.peek()? {
                    Some(b't') => b"true",
                    Some(b'f') => b"false",
                    _ => b"null",
                };
                for byte in expected {
                    self.expect(*byte)?;
                }
                Ok(Value::Null)
            }
            b'-' | b'0'..=b'9' => {
                self.number()?;
                Ok(Value::Null)
            }
            _ => anyhow::bail!("invalid JSON value"),
        }
    }

    fn object(&mut self, shape: Shape, depth: usize) -> Result<Value> {
        self.expect(b'{')?;
        self.space()?;
        let mut fields = serde_json::Map::new();
        let mut keys = HashSet::new();
        if self.peek()? == Some(b'}') {
            self.byte()?;
            return Ok(Value::Object(fields));
        }
        loop {
            let key = self
                .string(MAX_KEY_BYTES)?
                .context("JSON key exceeds its bound")?;
            ensure!(
                keys.len() < MAX_OBJECT_KEYS,
                "JSON object exceeds its key bound"
            );
            ensure!(keys.insert(key.clone()), "duplicate JSON object key");
            self.space()?;
            self.expect(b':')?;
            let selected = shape.field(&key);
            let value = self.value(selected, depth)?;
            if !matches!(selected, Shape::Ignore) {
                fields.insert(key, value);
            }
            self.space()?;
            match self.byte()? {
                b'}' => return Ok(Value::Object(fields)),
                b',' => self.space()?,
                _ => anyhow::bail!("invalid JSON object separator"),
            }
        }
    }

    fn array(&mut self, shape: Shape, depth: usize) -> Result<Value> {
        self.expect(b'[')?;
        self.space()?;
        let selected = matches!(shape, Shape::Content | Shape::Kinds);
        let mut values = Vec::new();
        if self.peek()? == Some(b']') {
            self.byte()?;
            return Ok(Value::Array(values));
        }
        loop {
            let item = if values.is_empty() {
                match shape {
                    Shape::Content => Shape::ContentItem,
                    Shape::Kinds => Shape::Text(128),
                    _ => Shape::Ignore,
                }
            } else {
                Shape::Ignore
            };
            let value = self.value(item, depth)?;
            // One item is permitted in a receipt; retaining a second suffices
            // to reject it. The rest is validated without growing this array.
            if selected && values.len() < 2 {
                values.push(value);
            }
            self.space()?;
            match self.byte()? {
                b']' => return Ok(Value::Array(values)),
                b',' => self.space()?,
                _ => anyhow::bail!("invalid JSON array separator"),
            }
        }
    }

    fn digits(&mut self) -> Result<()> {
        ensure!(
            matches!(self.peek()?, Some(b'0'..=b'9')),
            "missing JSON digit"
        );
        while matches!(self.peek()?, Some(b'0'..=b'9')) {
            self.byte()?;
        }
        Ok(())
    }

    fn number(&mut self) -> Result<()> {
        if self.peek()? == Some(b'-') {
            self.byte()?;
        }
        if self.peek()? == Some(b'0') {
            self.byte()?;
        } else {
            self.digits()?;
        }
        if self.peek()? == Some(b'.') {
            self.byte()?;
            self.digits()?;
        }
        if matches!(self.peek()?, Some(b'e' | b'E')) {
            self.byte()?;
            if matches!(self.peek()?, Some(b'+' | b'-')) {
                self.byte()?;
            }
            self.digits()?;
        }
        Ok(())
    }

    fn hex(&mut self) -> Result<u32> {
        let mut code = 0;
        for _ in 0..4 {
            code = code * 16
                + char::from(self.byte()?)
                    .to_digit(16)
                    .context("invalid JSON Unicode escape")?;
        }
        Ok(code)
    }

    fn string(&mut self, limit: usize) -> Result<Option<String>> {
        self.expect(b'"')?;
        let mut text = String::new();
        let mut oversized = false;
        loop {
            let byte = self.byte()?;
            let c = match byte {
                b'"' => return Ok((!oversized).then_some(text)),
                b'\\' => match self.byte()? {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'u' => {
                        let high = self.hex()?;
                        let code = if (0xd800..=0xdbff).contains(&high) {
                            self.expect(b'\\')?;
                            self.expect(b'u')?;
                            let low = self.hex()?;
                            ensure!(
                                (0xdc00..=0xdfff).contains(&low),
                                "invalid JSON surrogate pair"
                            );
                            0x10000 + ((high - 0xd800) << 10) + low - 0xdc00
                        } else {
                            high
                        };
                        char::from_u32(code).context("invalid JSON Unicode scalar")?
                    }
                    _ => anyhow::bail!("invalid JSON string escape"),
                },
                0x20..=0x7f => char::from(byte),
                0xc2..=0xf4 => {
                    let width = if byte < 0xe0 {
                        2
                    } else if byte < 0xf0 {
                        3
                    } else {
                        4
                    };
                    let mut bytes = [byte, 0, 0, 0];
                    for slot in &mut bytes[1..width] {
                        *slot = self.byte()?;
                    }
                    std::str::from_utf8(&bytes[..width])?
                        .chars()
                        .next()
                        .context("missing JSON Unicode scalar")?
                }
                _ => anyhow::bail!("invalid JSON string byte"),
            };
            if !oversized && text.len() + c.len_utf8() <= limit {
                text.push(c);
            } else {
                oversized = true;
                text.clear();
            }
        }
    }
}

pub(super) fn find(binding: &Binding, offer: &HookOffer) -> Result<Option<Receipt>> {
    match &offer.transcript {
        Some(snapshot) => snapshot.find(binding, &offer.context),
        None => Ok(None),
    }
}

fn receipt(value: &Value, binding: &Binding, context: &str) -> Result<Option<Receipt>> {
    let item = &value["payload"];
    let metadata = &item["internal_chat_message_metadata_passthrough"];
    if value["type"] != "response_item"
        || item["type"] != "message"
        || item["role"] != "developer"
        || metadata["content_item_kinds"] != serde_json::json!(["hooks.additional_context"])
    {
        return Ok(None);
    }
    let Some(content) = item["content"].as_array() else {
        return Ok(None);
    };
    if content.len() != 1
        || content[0]["type"] != "input_text"
        || content[0]["text"].as_str() != Some(context)
    {
        return Ok(None);
    }
    let id = |v: &Value| {
        v.as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
            .map(str::to_owned)
    };
    Ok(Some(Receipt {
        thread: binding.provider.session.clone(),
        turn: id(&metadata["turn_id"]).context("hook transcript lacks turn ID")?,
        item: id(&item["id"]).context("hook transcript lacks item ID")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{ProcessIdentity, ProviderGeneration};
    use std::io::Write;

    fn fixture() -> (tempfile::TempDir, Binding, PathBuf, Snapshot) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("sessions")).unwrap();
        let binding = Binding {
            agent: "a".into(),
            provider: ProviderGeneration {
                process: ProcessIdentity {
                    pid: 1,
                    started_at: chrono::Utc::now(),
                },
                session: "thread".into(),
                profile: root.to_string_lossy().into(),
            },
            socket: root.join("sock"),
            cwd: root.clone(),
            executable: root.join("codex"),
        };
        let path = root.join("sessions/rollout.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::json!({"type":"session_meta","payload":{"id":"thread","cwd":root}})
            ),
        )
        .unwrap();
        let snapshot = Snapshot::capture(&path, &binding).unwrap();
        (directory, binding, path, snapshot)
    }

    fn matching(context: &str) -> Value {
        serde_json::json!({"type":"response_item","payload":{"type":"message","id":"context-item","role":"developer","content":[{"type":"input_text","text":context}],"internal_chat_message_metadata_passthrough":{"turn_id":"active-turn","content_item_kinds":["hooks.additional_context"]}}})
    }

    fn append(path: &Path, bytes: &[u8]) {
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn append_matching(path: &Path, context: &str) {
        append(path, format!("{}\n", matching(context)).as_bytes());
    }

    // Write a real large record without constructing its body in test memory.
    fn compaction(path: &Path) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(
            br#"{"type":"compacted","payload":{"replacement_history":[{"content":[{"text":""#,
        )
        .unwrap();
        let block = [b'x'; 8192];
        for _ in 0..9 * 1024 * 1024 / block.len() {
            file.write_all(&block).unwrap();
        }
        file.write_all(b"\"}]}]}}\n").unwrap();
    }

    #[test]
    fn receipt_before_or_after_large_compaction_survives_restart_without_replay() {
        for before in [true, false] {
            let (_directory, binding, path, snapshot) = fixture();
            let context = "exact\n日本語 😀";
            if before {
                append_matching(&path, context);
            }
            compaction(&path);
            if !before {
                append_matching(&path, context);
            }
            assert!(path.metadata().unwrap().len() - snapshot.offset > MAX_SUFFIX);
            let receipt = snapshot.find(&binding, context).unwrap().unwrap();
            assert_eq!(receipt.item, "context-item");
            // The on-disk snapshot remains the original offer boundary. A
            // receiver restart produces the same evidence, never a fresh offer.
            let restored: Snapshot =
                serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
            assert_eq!(restored.find(&binding, context).unwrap(), Some(receipt));
        }
    }

    #[test]
    fn duplicates_separated_by_large_records_are_not_hidden_by_a_read_budget() {
        let (_directory, binding, path, snapshot) = fixture();
        append_matching(&path, "exact");
        compaction(&path);
        append_matching(&path, "exact");
        assert!(
            snapshot
                .find(&binding, "exact")
                .unwrap_err()
                .to_string()
                .contains("multiple")
        );
    }

    #[test]
    fn missing_receipt_stays_unresolved_after_large_growth_and_repeated_scans() {
        let (_directory, binding, path, snapshot) = fixture();
        compaction(&path);
        for _ in 0..2 {
            assert!(snapshot.find(&binding, "exact").unwrap().is_none());
        }
        // A non-matching over-limit developer text is not truncated into a
        // receipt. The following genuine receipt must still be examined.
        append_matching(&path, &"x".repeat(MAX_CONTEXT + 1));
        append_matching(&path, "exact");
        assert!(snapshot.find(&binding, "exact").unwrap().is_some());
    }

    #[test]
    fn projection_is_independent_of_field_order_and_small_read_boundaries() {
        let context = "quotes: \" \\ 日本語 😀\n";
        let expected = matching(context);
        // The type arrives after the payload, and Unicode escapes can straddle
        // every byte boundary. Capture cannot depend on seeing type first.
        let json = format!(
            "{{\"payload\":{},\"type\":\"response_item\"}}\n",
            expected["payload"]
        );
        for capacity in [1, 2, 7, 8192] {
            let mut reader = BufReader::with_capacity(capacity, json.as_bytes());
            let mut record = RecordReader {
                reader: &mut reader,
            };
            assert_eq!(record.value(Shape::Record, 0).unwrap(), expected);
            assert!(record.finish().unwrap());
        }
        let escaped = br#"{"payload":{"type":"message","id":"context-item","role":"developer","content":[{"type":"input_text","text":"\u65e5\u672c\u8a9e \ud83d\ude00"}],"internal_chat_message_metadata_passthrough":{"turn_id":"active-turn","content_item_kinds":["hooks.additional_context"]}},"type":"response_item"}"#;
        let mut reader = BufReader::with_capacity(1, escaped.as_slice());
        assert_eq!(
            RecordReader {
                reader: &mut reader
            }
            .value(Shape::Record, 0)
            .unwrap(),
            matching("日本語 😀")
        );
    }

    #[test]
    fn malformed_or_duplicate_fields_after_a_receipt_fail_closed() {
        let payload = matching("exact")["payload"].to_string();
        let invalid = [
            format!("{{\"payload\":{payload},\"type\":\"response_item\",\"type\":\"other\"}}"),
            format!(
                "{{\"type\":\"other\",\"payload\":{payload},\"t\\u0079pe\":\"response_item\"}}"
            ),
            r#"{"type":"other","payload":{"ignored":1,"ignored":2}}"#.into(),
            r#"{"type":"other","ignored":"\ud800"}"#.into(),
            r#"{"type":"other","ignored":"\q"}"#.into(),
            r#"{"type":"other","ignored":01}"#.into(),
            r#"{"type":"other","ignored":[1,]}"#.into(),
            r#"{"type":"other","ignored":{}} trailing"#.into(),
        ];
        for invalid in invalid {
            let (_directory, binding, path, snapshot) = fixture();
            append_matching(&path, "exact");
            append(&path, invalid.as_bytes());
            // An unfinished final line is ignored, as before, even if the
            // provider has not yet finished a currently malformed prefix.
            assert!(snapshot.find(&binding, "exact").unwrap().is_some());
            append(&path, b"\n");
            assert!(snapshot.find(&binding, "exact").is_err(), "{invalid}");
        }
    }

    #[test]
    fn oversized_strings_are_still_validated_and_multi_item_context_is_not_a_receipt() {
        let (_directory, binding, path, snapshot) = fixture();
        let mut multi = matching("exact");
        multi["payload"]["content"]
            .as_array_mut()
            .unwrap()
            .extend(std::iter::repeat_n(
                serde_json::json!({"type":"input_text","text":"extra"}),
                1000,
            ));
        append(&path, format!("{multi}\n").as_bytes());
        assert!(snapshot.find(&binding, "exact").unwrap().is_none());
        append_matching(&path, "exact");
        append(&path, br#"{"ignored":""#);
        append(&path, &vec![b'x'; MAX_CONTEXT + 1]);
        append(&path, b"\\q\"}\n");
        assert!(snapshot.find(&binding, "exact").is_err());

        let (_directory, binding, path, snapshot) = fixture();
        append_matching(&path, "exact");
        append(&path, b"{\"ignored\":\"\xff\"}\n");
        assert!(snapshot.find(&binding, "exact").is_err());
    }

    #[test]
    fn transcript_receipt_is_exact_bounded_and_bound_to_the_opened_file() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("sessions")).unwrap();
        let binding = Binding {
            agent: "a".into(),
            provider: ProviderGeneration {
                process: ProcessIdentity {
                    pid: 1,
                    started_at: chrono::Utc::now(),
                },
                session: "thread".into(),
                profile: root.to_string_lossy().into(),
            },
            socket: root.join("sock"),
            cwd: root.clone(),
            executable: root.join("codex"),
        };
        let path = root.join("sessions/rollout.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::json!({"type":"session_meta","payload":{"id":"thread","cwd":root}})
            ),
        )
        .unwrap();
        let snapshot = Snapshot::capture(&path, &binding).unwrap();
        let value = serde_json::json!({"type":"response_item","payload":{"type":"message","id":"context-item","role":"developer","content":[{"type":"input_text","text":"exact"}],"internal_chat_message_metadata_passthrough":{"turn_id":"active-turn","content_item_kinds":["hooks.additional_context"]}}});
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(file, "{value}").unwrap();
        assert!(snapshot.find(&binding, "exact").unwrap().is_none());
        writeln!(file).unwrap();
        assert_eq!(
            snapshot.find(&binding, "exact").unwrap().unwrap().turn,
            "active-turn"
        );
        assert!(snapshot.find(&binding, "partial").unwrap().is_none());
        let mut ordinary = value.clone();
        ordinary["payload"]["role"] = serde_json::json!("user");
        assert!(receipt(&ordinary, &binding, "exact").unwrap().is_none());
        ordinary = value.clone();
        ordinary["payload"]["internal_chat_message_metadata_passthrough"]["content_item_kinds"] =
            serde_json::json!(["ordinary"]);
        assert!(receipt(&ordinary, &binding, "exact").unwrap().is_none());
        writeln!(file, "{value}").unwrap();
        assert!(snapshot.find(&binding, "exact").is_err());
        drop(file);
        std::fs::rename(&path, root.join("old")).unwrap();
        std::fs::copy(root.join("old"), &path).unwrap();
        assert!(snapshot.find(&binding, "exact").is_err());
    }
}
