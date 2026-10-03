//! Resolve calls from a detached Codex app-server to an existing root binding.
//! Provider metadata selects a conversation only inside the verified host's
//! profile and checkout; it cannot create or substitute an agent identity.

use super::{Identity, McpArgs};
use agentdocker_core::{AgentRecord, ProcessIdentity, Request, Response};
use agentdocker_host::procinfo;
use anyhow::{Context as _, Result, ensure};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn profile_from_executable(executable: &Path) -> Result<PathBuf> {
    // The detached host filters CODEX_HOME from MCP environments. Its kernel
    // image, already canonicalized by the caller, identifies the private
    // package cache even when the user's default profile is different.
    ensure!(
        executable.is_absolute(),
        "Codex MCP host image is not absolute"
    );
    let parts: Vec<_> = executable.ancestors().take(6).collect();
    ensure!(
        parts.len() == 6
            && matches!(
                executable.file_name().and_then(|v| v.to_str()),
                Some("codex" | "codex.exe")
            )
            && parts[1].file_name().is_some_and(|v| v == "bin")
            && parts[3].file_name().is_some_and(|v| v == "releases")
            && parts[4]
                .file_name()
                .is_some_and(|v| v == "app-server-daemon")
            && parts[5].file_name().is_some_and(|v| v == "packages"),
        "detached Codex MCP host image is outside a provider package cache"
    );
    Ok(parts[5]
        .parent()
        .context("detached Codex MCP host cache has no provider profile")?
        .to_owned())
}

pub(super) struct Context {
    host: ProcessIdentity,
    executable: PathBuf,
    // A dedicated host proves its profile through the accepted remote ledger,
    // rather than inferring it from an arbitrary environment variable.
    profile: Option<PathBuf>,
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
        if !procinfo::is_codex_binary(&host.argv) || !host.argv.iter().any(|v| v == "app-server") {
            return Ok(None);
        }
        let cached = host.argv.iter().any(|v| v == "--managed-daemon");
        if !cached && !host.argv.iter().any(|v| v == "--listen") {
            return Ok(None);
        }
        let executable = procinfo::executable_path_of(pid)?.canonicalize()?;
        let profile = if cached {
            let profile = profile_from_executable(&executable)?.canonicalize()?;
            if let Some(configured) = std::env::var_os("CODEX_HOME") {
                ensure!(
                    PathBuf::from(configured).canonicalize()? == profile,
                    "detached Codex MCP host executable disagrees with its provider profile"
                );
            }
            Some(profile)
        } else {
            None
        };
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
        self.select(
            agents,
            meta,
            |process| procinfo::start_time(process.pid) == Some(process.started_at),
            |agent| {
                crate::codex_input::external::verify_mcp_host(
                    agent,
                    &self.host,
                    &self.executable,
                    &self.cwd,
                )
            },
        )
    }

    fn select(
        &self,
        agents: Vec<AgentRecord>,
        meta: &Value,
        alive: impl Fn(&ProcessIdentity) -> bool,
        dedicated: impl Fn(&AgentRecord) -> Result<()>,
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
                && self
                    .profile
                    .as_ref()
                    .is_none_or(|profile| Path::new(&provider.profile) == profile)
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
        if self.profile.is_none() {
            dedicated(&agent)?;
        }
        Ok(agent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, InputBinding, ProviderGeneration};
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn native_identity_profile_comes_only_from_the_host_package_cache() {
        let root = std::env::temp_dir().join("private-codex-profile");
        for binary in ["codex", "codex.exe"] {
            let executable = root
                .join("packages/app-server-daemon/releases/0.160.0-target/bin")
                .join(binary);
            assert_eq!(profile_from_executable(&executable).unwrap(), root);
        }
        for suffix in [
            "bin/codex",
            "packages/other-server/releases/version/bin/codex",
            "packages/app-server-daemon/releases/version/other/codex",
            "packages/app-server-daemon/releases/version/bin/other",
        ] {
            assert!(profile_from_executable(&root.join(suffix)).is_err());
        }
        assert!(profile_from_executable(Path::new("relative/bin/codex")).is_err());
    }

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
            profile: Some("/profile".into()),
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
                .select(vec![agent.clone()], &meta, |_| true, |_| Ok(()))
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
                    .select(vec![agent.clone()], &invalid, |_| true, |_| Ok(()))
                    .is_err()
            );
        }
        assert!(context.select(vec![], &meta, |_| true, |_| Ok(())).is_err());
        assert!(
            context
                .select(vec![agent.clone(), agent], &meta, |_| true, |_| Ok(()))
                .is_err()
        );
    }

    #[test]
    fn dedicated_identity_requires_accepted_host_proof_after_root_selection() {
        let (mut context, agent, meta) = fixture();
        context.profile = None;
        let calls = std::cell::Cell::new(0);
        let proof = |selected: &AgentRecord| {
            calls.set(calls.get() + 1);
            ensure!(selected.id == agent.id, "unexpected selection");
            Ok(())
        };
        assert_eq!(
            context
                .select(vec![agent.clone()], &meta, |_| true, proof)
                .unwrap()
                .id,
            agent.id
        );
        assert_eq!(calls.get(), 1);
        for invalid in [
            Value::Null,
            json!({"threadId":"child-thread","sessionId":"root-thread"}),
        ] {
            assert!(
                context
                    .select(vec![agent.clone()], &invalid, |_| true, proof)
                    .is_err()
            );
        }
        assert_eq!(calls.get(), 1);
        assert!(
            context
                .select(
                    vec![agent.clone()],
                    &meta,
                    |_| true,
                    |_| { anyhow::bail!("server birth or capability record changed") }
                )
                .is_err()
        );
        assert!(
            context
                .select(vec![agent.clone(), agent.clone()], &meta, |_| true, proof)
                .is_err()
        );
        assert_eq!(calls.get(), 1);
        // Cached detached hosts retain their existing profile proof and do not
        // depend on a dedicated-server descriptor that those versions lack.
        context.profile = Some("/profile".into());
        assert!(
            context
                .select(
                    vec![agent],
                    &meta,
                    |_| true,
                    |_| { anyhow::bail!("dedicated proof must not run for a cached host") }
                )
                .is_ok()
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
            assert!(
                context
                    .select(vec![wrong], &meta, |_| true, |_| Ok(()))
                    .is_err()
            );
        }
        assert!(
            context
                .select(vec![agent], &meta, |_| false, |_| Ok(()))
                .is_err()
        );
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
            Request::Register { .. }
                | Request::Deregister { .. }
                | Request::Inspect { .. }
                | Request::Ask { .. }
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
