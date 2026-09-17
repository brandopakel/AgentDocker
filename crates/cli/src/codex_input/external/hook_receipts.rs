//! Codex 0.154 persists Pre/PostToolUse context as a tagged developer message,
//! omitted by thread/items/list. Read only the bounded suffix after our offer.
use super::{
    super::ledger::Receipt,
    ledger::{Binding, HookOffer},
};
use agentdocker_host::dirs;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const MAX_SUFFIX: u64 = 4 * 1024 * 1024;

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
            device: meta.dev(),
            inode: meta.ino(),
            offset,
        })
    }

    pub fn find(&self, binding: &Binding, context: &str) -> Result<Option<Receipt>> {
        let mut file = open(&self.path, binding)?;
        let meta = file.metadata()?;
        ensure!(
            meta.dev() == self.device && meta.ino() == self.inode && meta.len() >= self.offset,
            "hook transcript was replaced or truncated"
        );
        let length = meta.len() - self.offset;
        ensure!(
            length <= MAX_SUFFIX,
            "hook transcript receipt exceeds bounded recovery suffix"
        );
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file.take(length));
        let mut bytes = Vec::new();
        let mut found = None;
        loop {
            bytes.clear();
            if reader.read_until(b'\n', &mut bytes)? == 0 {
                break;
            }
            if bytes.last() != Some(&b'\n') {
                break;
            } // Provider still writing.
            let value: Value =
                serde_json::from_slice(&bytes).context("invalid hook transcript record")?;
            if let Some(receipt) = receipt(&value, binding, context)? {
                ensure!(
                    found.is_none(),
                    "multiple transcript receipts match one hook offer"
                );
                found = Some(receipt);
            }
        }
        let current = std::fs::symlink_metadata(&self.path)?;
        ensure!(
            current.is_file()
                && current.dev() == self.device
                && current.ino() == self.inode
                && current.len() >= meta.len(),
            "hook transcript changed while reading"
        );
        Ok(found)
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
