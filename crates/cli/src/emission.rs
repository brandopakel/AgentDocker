//! Output accounting is best effort, bounded, metadata-only, and never changes
//! delivery/ACK behavior. A successful write is not proof of model consumption.
use crate::client::Backend;
use agentdocker_core::usage::emission::{Channel, Emission};
use serde_json::Value;

pub(crate) async fn record(backend: &impl Backend, agent: &str, channel: Channel, bytes: usize) {
    if bytes == 0 {
        return;
    }
    let emission = Emission {
        id: uuid::Uuid::new_v4().simple().to_string(),
        channel,
        bytes: bytes as u64,
    };
    if emission.check().is_ok() {
        backend.record_emission(agent, emission).await;
    }
}

/// Match the compact JSON and trailing newline actually written by hooks/MCP.
pub(crate) async fn json_line(
    backend: &impl Backend,
    agent: &str,
    channel: Channel,
    value: &Value,
) {
    if let Ok(bytes) = serde_json::to_vec(value) {
        record(backend, agent, channel, bytes.len() + 1).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Recorder(RefCell<Vec<(String, Emission)>>);
    impl Backend for Recorder {
        async fn call(
            &self,
            _: agentdocker_core::Request,
        ) -> anyhow::Result<agentdocker_core::Response> {
            panic!("metadata reporting does not make a model/tool call")
        }
        async fn record_emission(&self, agent: &str, emission: Emission) {
            self.0.borrow_mut().push((agent.into(), emission));
        }
    }

    #[tokio::test]
    async fn counts_utf8_frame_bytes_and_distinguishes_repeated_writes() {
        let backend = Recorder(RefCell::new(Vec::new()));
        let value = serde_json::json!({"content":"é界🙂\\n"});
        for _ in 0..2 {
            json_line(&backend, "receiver", Channel::Mcp, &value).await;
        }
        record(&backend, "receiver", Channel::Hook, 0).await;
        let records = backend.0.borrow();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, "receiver");
        assert_eq!(
            records[0].1.bytes,
            (serde_json::to_vec(&value).unwrap().len() + 1) as u64
        );
        assert_ne!(records[0].1.id, records[1].1.id);
        assert_eq!(records[0].1.channel, Channel::Mcp);
        assert!(records.iter().all(|(_, e)| e.check().is_ok()));
    }
}
