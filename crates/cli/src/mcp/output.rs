//! Request-local identity follows a reply to its completed stdio write. Shared
//! native hosts never cache a caller or infer one from output/model text.
use super::*;
use agentdocker_core::usage::emission::Channel;

enum Attribution {
    Whole(String),
    Parts(BTreeMap<String, usize>),
    Unknown,
}

pub(super) struct Reply {
    pub(super) value: Value,
    attribution: Attribution,
}

impl Reply {
    pub(super) fn single(value: Value, agent: Option<String>) -> Self {
        Self {
            value,
            attribution: agent
                .filter(|id| !id.is_empty())
                .map(Attribution::Whole)
                .unwrap_or(Attribution::Unknown),
        }
    }

    pub(super) fn batch(replies: Vec<Self>) -> Option<Self> {
        let first = replies.first()?;
        let common = match &first.attribution {
            Attribution::Whole(agent)
                if replies.iter().all(|reply| {
                    matches!(&reply.attribution, Attribution::Whole(other) if other == agent)
                }) => Some(agent.clone()),
            _ => None,
        };
        let mut parts = BTreeMap::<String, usize>::new();
        if common.is_none() {
            // Mixed/unverified batches cannot assign shared brackets, commas
            // or the trailing newline. Count only each verified JSON segment.
            for reply in &replies {
                if let Ok(bytes) = serde_json::to_vec(&reply.value) {
                    for (agent, bytes) in reply.counts(bytes.len()) {
                        *parts.entry(agent).or_default() += bytes;
                    }
                }
            }
        }
        Some(Self {
            value: Value::Array(replies.into_iter().map(|reply| reply.value).collect()),
            attribution: common
                .map(Attribution::Whole)
                .unwrap_or(Attribution::Parts(parts)),
        })
    }

    fn counts(&self, whole_bytes: usize) -> BTreeMap<String, usize> {
        match &self.attribution {
            Attribution::Whole(agent) => BTreeMap::from([(agent.clone(), whole_bytes)]),
            Attribution::Parts(parts) => parts.clone(),
            Attribution::Unknown => BTreeMap::new(),
        }
    }

    pub(super) async fn write(
        &self,
        backend: &impl Backend,
        output: &mut (impl tokio::io::AsyncWrite + Unpin),
    ) -> Result<()> {
        let bytes = super::write_line(output, &self.value).await?;
        // Bound the whole frame's reporting, including a batch for several
        // agents. No failed/partial output is counted, replayed or acknowledged.
        let _ = tokio::time::timeout(Duration::from_millis(50), async {
            for (agent, count) in self.counts(bytes) {
                crate::emission::record(backend, &agent, Channel::Mcp, count).await;
            }
        })
        .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::usage::emission::Emission;
    use std::cell::RefCell;
    use tokio::io::AsyncReadExt;

    #[derive(Default)]
    struct Recorder(RefCell<Vec<(String, Emission)>>);
    impl Backend for Recorder {
        async fn call(&self, _: Request) -> Result<Response> {
            panic!("writing a response must not resolve or change identity")
        }
        async fn record_emission(&self, agent: &str, emission: Emission) {
            self.0.borrow_mut().push((agent.to_owned(), emission));
        }
    }

    fn reply(id: u64, text: &str, agent: Option<&str>) -> Reply {
        Reply::single(
            json!({"jsonrpc":"2.0","id":id,"result":{"text":text}}),
            agent.map(str::to_owned),
        )
    }

    #[tokio::test]
    async fn completed_utf8_output_is_counted_once_and_failed_output_is_not() {
        let backend = Recorder::default();
        let reply = reply(1, "é界🙂", Some("verified"));
        let (mut output, mut input) = tokio::io::duplex(1024);
        reply.write(&backend, &mut output).await.unwrap();
        drop(output);
        let mut observed = Vec::new();
        input.read_to_end(&mut observed).await.unwrap();
        let mut expected = serde_json::to_vec(&reply.value).unwrap();
        expected.push(b'\n');
        assert_eq!(observed, expected);
        let (mut broken, peer) = tokio::io::duplex(1);
        drop(peer);
        assert!(reply.write(&backend, &mut broken).await.is_err());
        let rows = backend.0.borrow();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "verified");
        assert_eq!(rows[0].1.bytes, observed.len() as u64);
        assert_eq!(rows[0].1.channel, Channel::Mcp);
    }

    #[tokio::test]
    async fn mixed_batch_keeps_each_agent_separate_and_excludes_unverified_framing() {
        let backend = Recorder::default();
        let a = reply(1, "é界🙂", Some("a"));
        let b = reply(2, "other", Some("b"));
        let again = reply(3, "same session", Some("a"));
        let expected_a = serde_json::to_vec(&a.value).unwrap().len()
            + serde_json::to_vec(&again.value).unwrap().len();
        let expected_b = serde_json::to_vec(&b.value).unwrap().len();
        let unknown = Reply::single(json!({"emission_agent":"forged"}), None);
        let batch = Reply::batch(vec![a, b, again, unknown]).unwrap();
        let mut output = Vec::new();
        batch.write(&backend, &mut output).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            batch.value
        );
        let rows = backend.0.borrow();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "a");
        assert_eq!(rows[0].1.bytes, expected_a as u64);
        assert_eq!(rows[1].0, "b");
        assert_eq!(rows[1].1.bytes, expected_b as u64);
        assert_ne!(rows[0].1.id, rows[1].1.id);
        assert!(expected_a + expected_b < output.len());
    }

    #[tokio::test]
    async fn one_agent_batch_includes_framing_and_unknown_next_frame_inherits_nothing() {
        let backend = Recorder::default();
        let batch =
            Reply::batch(vec![reply(1, "one", Some("a")), reply(2, "two", Some("a"))]).unwrap();
        let mut output = Vec::new();
        batch.write(&backend, &mut output).await.unwrap();
        let bytes = output.len();
        reply(3, "unknown", None)
            .write(&backend, &mut output)
            .await
            .unwrap();
        reply(4, "empty identity", Some(""))
            .write(&backend, &mut output)
            .await
            .unwrap();
        let rows = backend.0.borrow();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.bytes, bytes as u64);
        assert!(Reply::batch(Vec::new()).is_none());
    }

    #[tokio::test]
    async fn a_stalled_report_cannot_hold_the_completed_frame_indefinitely() {
        struct Stalled;
        impl Backend for Stalled {
            async fn call(&self, _: Request) -> Result<Response> {
                unreachable!()
            }
            async fn record_emission(&self, _: &str, _: Emission) {
                std::future::pending::<()>().await;
            }
        }
        let batch =
            Reply::batch(vec![reply(1, "one", Some("a")), reply(2, "two", Some("b"))]).unwrap();
        let mut output = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), batch.write(&Stalled, &mut output))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            batch.value
        );
    }
}
