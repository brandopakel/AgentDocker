//! Resolve calls from a detached Codex app-server to an existing root binding.
//! Provider metadata selects a conversation only inside the verified host's
//! profile and checkout; it cannot create or substitute an agent identity.

use super::{Identity, McpArgs};
use agentdocker_core::{AgentRecord, ProcessIdentity, Request, Response};
use agentdocker_host::procinfo;
use anyhow::{Context as _, Result, ensure};
use serde_json::Value;
use std::path::PathBuf;

pub(super) struct Context {
    host: ProcessIdentity,
    executable: PathBuf,
    profile: PathBuf,
    cwd: PathBuf,
}

impl Context {
    pub(super) fn detect(args: &McpArgs) -> Result<Option<Self>> {
        if args.runtime != "codex"
            || args.pid.is_some()
            || std::env::var("AGENTDOCKER_AGENT_ID").is_ok_and(|id| !id.is_empty())
            || std::env::var(agentdocker_host::provider_input::CODEX_INPUT_ENV).as_deref()
                == Ok("1")
        {
            return Ok(None);
        }
        let pid = super::parent_id();
        let table = procinfo::processes()?;
        let Some(host) = table.iter().find(|p| p.pid == pid) else {
            return Ok(None);
        };
        if !procinfo::is_codex_binary(&host.argv)
            || !host.argv.iter().any(|v| v == "app-server")
            || !host.argv.iter().any(|v| v == "--managed-daemon")
        {
            return Ok(None);
        }
        let profile = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex")))
            .context("detached Codex MCP host has no provider profile")?
            .canonicalize()?;
        let executable = procinfo::executable_path_of(pid)?.canonicalize()?;
        ensure!(
            executable.starts_with(profile.join("packages/app-server-daemon/releases")),
            "detached Codex MCP host executable is outside its provider profile"
        );
        Ok(Some(Self {
            host: ProcessIdentity {
                pid,
                started_at: procinfo::start_time(pid)
                    .context("Codex MCP host birth unavailable")?,
            },
            executable,
            profile,
            cwd: std::env::current_dir()?.canonicalize()?,
        }))
    }

    /// No helper registration is made: initialization precedes the first hook.
    pub(super) fn unbound_identity(&self) -> Identity {
        Identity {
            id: String::new(),
            name: "Codex conversation awaiting a native binding".into(),
            registered_here: false,
            host_pid: Some(self.host.pid),
            host_started_at: Some(self.host.started_at),
        }
    }

    pub(super) async fn resolve<B: super::Backend>(
        &self,
        backend: &B,
        meta: &Value,
    ) -> Result<AgentRecord> {
        ensure!(
            procinfo::start_time(self.host.pid) == Some(self.host.started_at)
                && procinfo::executable_path_of(self.host.pid)?.canonicalize()? == self.executable,
            "detached Codex MCP host identity changed"
        );
        let Response::Agents { agents, .. } = backend
            .call(Request::List {
                all: true,
                project: None,
                labels: Default::default(),
            })
            .await?
        else {
            anyhow::bail!("cannot inspect native Codex bindings");
        };
        self.select(agents, meta, |process| {
            procinfo::start_time(process.pid) == Some(process.started_at)
        })
    }

    fn select(
        &self,
        agents: Vec<AgentRecord>,
        meta: &Value,
        alive: impl Fn(&ProcessIdentity) -> bool,
    ) -> Result<AgentRecord> {
        // Codex 0.160 supplies these outside model-controlled tool arguments.
        // A child's thread differs from its root session; never route it into
        // that root's queue or silently fall back to the app-server identity.
        let thread = meta["threadId"]
            .as_str()
            .context("Codex tool call lacks threadId metadata")?;
        ensure!(
            !thread.is_empty()
                && thread.len() <= 256
                && !thread.chars().any(char::is_control)
                && meta["sessionId"].as_str() == Some(thread),
            "Codex tool call must identify its own root conversation"
        );
        let mut matches = agents.into_iter().filter(|agent| {
            let Some(binding) = &agent.input_binding else {
                return false;
            };
            let provider = &binding.provider;
            agent.spec.runtime == "codex"
                && provider.session == thread
                && std::path::Path::new(&provider.profile) == self.profile
                && agent.spec.workdir.as_ref() == Some(&self.cwd)
                && agent.pid == Some(provider.process.pid)
                && agent.process_started_at == Some(provider.process.started_at)
                && alive(&provider.process)
        });
        let agent = matches.next().context(
            "Codex conversation has no live native binding in this profile and checkout",
        )?;
        ensure!(
            matches.next().is_none(),
            "Codex conversation binding is ambiguous"
        );
        Ok(agent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, InputBinding, ProviderGeneration};
    use chrono::Utc;
    use serde_json::json;

    fn fixture() -> (Context, AgentRecord, Value) {
        let now = Utc::now();
        let process = ProcessIdentity {
            pid: 42,
            started_at: now,
        };
        let context = Context {
            host: ProcessIdentity {
                pid: 43,
                started_at: now,
            },
            executable: "/profile/server".into(),
            profile: "/profile".into(),
            cwd: "/project".into(),
        };
        let mut agent = AgentRecord::new(
            AgentSpec {
                runtime: "codex".into(),
                workdir: Some(context.cwd.clone()),
                ..AgentSpec::default()
            },
            false,
            now,
        );
        agent.pid = Some(process.pid);
        agent.process_started_at = Some(now);
        agent.input_binding = Some(InputBinding {
            provider: ProviderGeneration {
                process,
                session: "root-thread".into(),
                profile: "/profile".into(),
            },
            controller: context.host.clone(),
            controller_since: now,
            token_sha256: "fixture".into(),
            bound_at: now,
            controller_generations: 1,
            uncertain: vec![],
            launch: None,
            restart: Default::default(),
        });
        (
            context,
            agent,
            json!({"threadId":"root-thread","sessionId":"root-thread"}),
        )
    }

    #[test]
    fn native_identity_requires_root_metadata_and_one_matching_binding() {
        let (context, agent, meta) = fixture();
        assert_eq!(
            context
                .select(vec![agent.clone()], &meta, |_| true)
                .unwrap()
                .id,
            agent.id
        );
        for invalid in [
            Value::Null,
            json!({"threadId":"root-thread"}),
            json!({"threadId":"child-thread","sessionId":"root-thread"}),
            json!({"threadId":"other-root","sessionId":"other-root"}),
        ] {
            assert!(
                context
                    .select(vec![agent.clone()], &invalid, |_| true)
                    .is_err()
            );
        }
        assert!(context.select(vec![], &meta, |_| true).is_err());
        assert!(
            context
                .select(vec![agent.clone(), agent], &meta, |_| true)
                .is_err()
        );
    }

    #[test]
    fn native_identity_refuses_other_profiles_checkouts_and_process_generations() {
        let (context, agent, meta) = fixture();
        let mut variants = vec![];
        let mut wrong = agent.clone();
        wrong.input_binding = None;
        variants.push(wrong);
        let mut wrong = agent.clone();
        wrong.spec.runtime = "claude-code".into();
        variants.push(wrong);
        let mut wrong = agent.clone();
        wrong.spec.workdir = Some("/other-project".into());
        variants.push(wrong);
        let mut wrong = agent.clone();
        wrong.input_binding.as_mut().unwrap().provider.profile = "/other-profile".into();
        variants.push(wrong);
        let mut wrong = agent.clone();
        wrong.pid = Some(100);
        variants.push(wrong);
        let mut wrong = agent.clone();
        wrong.process_started_at = None;
        variants.push(wrong);
        let mut wrong = agent.clone();
        wrong.process_started_at = Some(Utc::now() + chrono::Duration::seconds(1));
        variants.push(wrong);
        for wrong in variants {
            assert!(context.select(vec![wrong], &meta, |_| true).is_err());
        }
        assert!(context.select(vec![agent], &meta, |_| false).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_call_posts_as_the_bound_conversation_and_keeps_delivery_owned() {
        use crate::{client::mock::Mock, mcp::McpServer};
        let (mut context, mut agent, meta) = fixture();
        let pid = std::process::id();
        let birth = procinfo::start_time(pid).unwrap();
        context.host = ProcessIdentity {
            pid,
            started_at: birth,
        };
        context.executable = procinfo::executable_path().unwrap().canonicalize().unwrap();
        agent.pid = Some(pid);
        agent.process_started_at = Some(birth);
        agent.input_binding.as_mut().unwrap().provider.process = context.host.clone();
        let backend = Mock::with(vec![
            Response::Agents {
                agents: vec![agent.clone()],
                aliases: Default::default(),
            },
            Response::Ok,
            Response::Agent {
                agent: agent.clone(),
            },
            Response::Sent {
                message: "question".to_owned().into(),
                subscribers: 0,
                recipient_readiness: None,
            },
        ]);
        let mut server = McpServer::new(backend, context.unbound_identity());
        server.native_context = Some(context);
        let result = server
            .call_tool(
                json!({"name":"ask_human", "arguments":{"question":"Fixture?"}, "_meta":meta}),
            )
            .await
            .unwrap();
        let value: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(value["answer_delivery"], "native_queue");
        assert!(
            server.backend.requests().iter().any(
                |r| matches!(r, Request::PostQuestion { from, .. } if from == agent.id.as_str())
            )
        );
        server.shutdown().await;
        assert!(!server.backend.requests().iter().any(|r| matches!(
            r,
            Request::Register { .. } | Request::Deregister { .. } | Request::Ask { .. }
        )));
        let mut bound = McpServer::new(
            Mock::default(),
            Identity {
                id: agent.id.to_string(),
                name: "root".into(),
                registered_here: false,
                host_pid: Some(pid),
                host_started_at: Some(birth),
            },
        );
        bound.native_input = true;
        for tool in ["read_inbox", "wait_for_messages", "acknowledge_messages"] {
            assert!(bound.tool(tool, json!({})).await.is_err());
        }
        assert!(bound.backend.requests().is_empty());
    }
}
