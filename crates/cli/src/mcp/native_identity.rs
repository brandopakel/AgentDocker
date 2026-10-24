//! Resolve calls from a detached Codex app-server to an existing root binding.
//! Provider metadata selects a conversation only inside the verified host's
//! profile and checkout; it cannot create or substitute an agent identity.
//!
//! A conversation started in a terminal has no binding: since Codex 0.162 its
//! hooks and MCP servers run in the shared app-server, not under the terminal.
//! [`Context::resolve_terminal`] finds it by the terminal that resumed it.

use super::{Identity, McpArgs};
use agentdocker_core::{AgentRecord, AgentSpec, ProcessIdentity, Request, Response};
use agentdocker_host::procinfo;
use anyhow::{Context as _, Result, bail, ensure};
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
    agentdocker_host::codex_control::shared_server_profile(executable)
        .context("detached Codex MCP host image is outside a provider package cache")
}

/// Mark only backend I/O failures; identity checks keep their original causes.
#[derive(Debug)]
struct TransportError(anyhow::Error);

impl std::fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// No binding names this conversation: a terminal session's, or one that
/// has not bound yet. Distinct from a binding that exists and was refused.
#[derive(Debug)]
pub(crate) struct Unbound;

impl std::fmt::Display for Unbound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("Codex conversation has no live native binding in this profile and checkout")
    }
}

impl std::error::Error for Unbound {}

/// Whether `error` is only the absence of a binding.
pub(crate) fn unbound(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Unbound>().is_some()
}

/// The root conversation a tool call or hook names. Codex supplies these
/// outside model-controlled tool arguments. 0.155.1 carries the root session
/// in x-codex-turn-metadata, while 0.160 also supplies sessionId. Require all
/// supplied identities to agree; a malformed newer field cannot fall back to
/// the older one. A child's thread differs from its root session; never route
/// it into that root's queue or silently fall back to the app-server identity.
fn root_thread(meta: &Value) -> Result<&str> {
    let thread = meta["threadId"]
        .as_str()
        .context("Codex tool call lacks threadId metadata")?;
    let turn_metadata = meta.get("x-codex-turn-metadata");
    let session = match meta.get("sessionId") {
        Some(value) => value.as_str(),
        None => turn_metadata.and_then(|value| value["session_id"].as_str()),
    };
    ensure!(
        !thread.is_empty()
            && thread.len() <= 256
            && !thread.chars().any(char::is_control)
            && session == Some(thread)
            && turn_metadata.is_none_or(|value| {
                value["thread_id"].as_str() == Some(thread)
                    && value["session_id"].as_str() == Some(thread)
            }),
        "Codex tool call must identify its own root conversation"
    );
    Ok(thread)
}

/// Whether a tool call's `_meta` names a Codex conversation: a `threadId`,
/// or `x-codex-turn-metadata` naming a thread or session — the shapes
/// [`root_thread`] reads, which every Codex that sends a session sends.
/// Positive evidence only: a bare `sessionId`, a name other clients may
/// use too, is not taken for Codex on its own.
pub(crate) fn names_codex_conversation(meta: &Value) -> bool {
    let named = |value: &Value| value.as_str().is_some_and(|v| !v.trim().is_empty());
    named(&meta["threadId"])
        || meta
            .get("x-codex-turn-metadata")
            .is_some_and(|turn| named(&turn["thread_id"]) || named(&turn["session_id"]))
}

/// Preserve the established transport diagnostic without mislabelling bindings.
pub(super) fn mcp_error(error: anyhow::Error) -> (i64, String) {
    match error.downcast::<TransportError>() {
        Ok(TransportError(cause)) => super::transport(cause),
        Err(cause) => (
            super::INTERNAL_ERROR,
            format!("cannot resolve native Codex identity: {cause:#}"),
        ),
    }
}

pub(crate) struct Context {
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
        Self::from_host(pid, std::env::current_dir()?.canonicalize()?, &table)
    }

    /// Hooks stop at the first Codex host just as MCP does. Never climb past a
    /// server into an unrelated launcher or containing provider session.
    pub(crate) fn from_host(
        pid: u32,
        cwd: PathBuf,
        table: &[procinfo::Process],
    ) -> Result<Option<Self>> {
        let Some(host) = table.iter().find(|p| p.pid == pid) else {
            return Ok(None);
        };
        // Reuse the daemon's cached-host predicate while also supporting the
        // explicitly authenticated dedicated server accepted by this receiver.
        let cached = procinfo::detached_codex_app_server(&host.argv);
        // --program accepts an explicitly selected native image under any
        // filename. This shape only opts into binding resolution: the kernel
        // image/birth and accepted server record still prove authority below.
        let dedicated = host.argv.iter().any(|v| v == "app-server")
            && host.argv.iter().any(|v| v == "--listen");
        if !cached && !dedicated {
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
            cwd,
        }))
    }

    /// The profile the shared cached host proves by its package cache; none
    /// for a dedicated server, whose profile comes from its accepted binding.
    pub(crate) fn shared_profile(&self) -> Option<&Path> {
        self.profile.as_deref()
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

    fn verify_host(&self) -> Result<()> {
        ensure!(
            procinfo::start_time(self.host.pid) == Some(self.host.started_at)
                && procinfo::executable_path_of(self.host.pid)?.canonicalize()? == self.executable,
            "detached Codex MCP host identity changed"
        );
        Ok(())
    }

    /// Verify the host and select its existing binding, distinguishing backend I/O.
    pub(crate) async fn resolve<B: super::Backend>(
        &self,
        backend: &B,
        meta: &Value,
    ) -> Result<AgentRecord> {
        self.verify_host()?;
        let Response::Agents { agents, .. } = backend
            .call(Request::List {
                all: true,
                project: None,
                labels: Default::default(),
            })
            .await
            .map_err(TransportError)?
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
        let thread = root_thread(meta)?;
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
        let agent = matches.next().ok_or_else(|| anyhow::Error::new(Unbound))?;
        ensure!(
            matches.next().is_none(),
            "Codex conversation binding is ambiguous"
        );
        if self.profile.is_none() {
            dedicated(&agent)?;
        }
        Ok(agent)
    }

    /// The record of the terminal that resumed this conversation, bound to
    /// it: for a conversation with no binding, whose hooks and MCP servers
    /// run in the shared app-server rather than under its terminal. The
    /// terminal is the one live `codex resume <thread>` in this checkout;
    /// what was running a conversation the hook names is its proof. A
    /// conversation started without naming its thread (`codex` alone) has
    /// none, and stays [`Unbound`].
    pub(crate) async fn resolve_terminal<B: super::Backend>(
        &self,
        backend: &B,
        meta: &Value,
    ) -> Result<AgentRecord> {
        // Only the shared cached app-server, whose package cache proves its
        // profile: a dedicated server earns authority only through the
        // binding it was accepted with, which a terminal does not have.
        if self.profile.is_none() {
            return Err(anyhow::Error::new(Unbound));
        }
        self.verify_host()?;
        let thread = root_thread(meta)?;
        let table = procinfo::processes()?;
        let terminal = self.terminal(
            &table,
            thread,
            procinfo::cwd,
            procinfo::arguments,
            procinfo::start_time,
        )?;
        bind_terminal(
            backend,
            &terminal,
            thread,
            &self.cwd,
            self.profile.as_deref(),
        )
        .await
    }

    /// The record of a conversation that no binding names and no terminal
    /// proves, found or made by its key: this profile, its root thread.
    /// The conversation is the identity; it has no process here, and a
    /// process proven later joins the same record. Only the shared cached
    /// host, whose package cache proves its profile, may name one. A
    /// binding that names the conversation but cannot be used here is not
    /// "nothing names it": that is its controller's to recover, never a
    /// second way into its queue.
    pub(crate) async fn attach<B: super::Backend>(
        &self,
        backend: &B,
        meta: &Value,
        adapter: agentdocker_core::AdapterKind,
    ) -> Result<AgentRecord> {
        if self.profile.is_none() {
            return Err(anyhow::Error::new(Unbound));
        }
        self.verify_host()?;
        self.attach_verified(backend, meta, adapter).await
    }

    /// [`Self::attach`] once the host is verified.
    async fn attach_verified<B: super::Backend>(
        &self,
        backend: &B,
        meta: &Value,
        adapter: agentdocker_core::AdapterKind,
    ) -> Result<AgentRecord> {
        let profile = self
            .profile
            .as_ref()
            .ok_or_else(|| anyhow::Error::new(Unbound))?;
        let thread = root_thread(meta)?;
        let Response::Agents { agents, .. } = backend
            .call(Request::List {
                all: true,
                project: None,
                labels: Default::default(),
            })
            .await
            .map_err(TransportError)?
        else {
            bail!("cannot inspect native Codex bindings");
        };
        ensure!(
            !bound_elsewhere(&agents, thread),
            "this Codex conversation has an input binding that cannot be used here; recover it from its session"
        );
        match backend
            .call(Request::SessionAttach {
                runtime: "codex".into(),
                profile: profile.display().to_string(),
                session: thread.to_owned(),
                workdir: self.cwd.clone(),
                adapter,
            })
            .await
            .map_err(TransportError)?
        {
            Response::Agent { agent } => Ok(agent),
            Response::Error { message, .. } => bail!("the conversation cannot attach: {message}"),
            _ => bail!("unexpected response attaching the conversation"),
        }
    }

    fn terminal(
        &self,
        table: &[procinfo::Process],
        thread: &str,
        cwd: impl Fn(u32) -> Option<PathBuf>,
        arguments: impl Fn(u32) -> Option<Vec<String>>,
        birth: impl Fn(u32) -> Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<ProcessIdentity> {
        let launchers = procinfo::codex_launchers(table);
        // The process table's text finds the candidates; their own whole
        // arguments decide, so a prompt reading `resume <thread>` is none.
        let mut found = table.iter().filter(|process| {
            !launchers.contains(&process.pid)
                && procinfo::codex_resumes(&process.argv, thread)
                && arguments(process.pid).is_some_and(|argv| procinfo::codex_resumes(&argv, thread))
                && cwd(process.pid)
                    .and_then(|dir| dir.canonicalize().ok())
                    .as_ref()
                    == Some(&self.cwd)
        });
        let Some(terminal) = found.next() else {
            return Err(anyhow::Error::new(Unbound));
        };
        ensure!(
            found.next().is_none(),
            "more than one Codex terminal resumed this conversation"
        );
        Ok(ProcessIdentity {
            pid: terminal.pid,
            started_at: birth(terminal.pid).context("Codex terminal birth unavailable")?,
        })
    }
}

/// The live record of `terminal` in `checkout`, bound to `thread`: an
/// existing record learns its conversation (and the daemon may fold it into
/// the record that held it before), or one is registered. A record bound to
/// another conversation is refused.
/// Whether some record's input binding names this conversation, live or
/// not: its controller owns the conversation's delivery.
fn bound_elsewhere(agents: &[AgentRecord], thread: &str) -> bool {
    agents.iter().any(|agent| {
        agent
            .input_binding
            .as_ref()
            .is_some_and(|binding| binding.provider.session == thread)
    })
}

async fn bind_terminal<B: super::Backend>(
    backend: &B,
    terminal: &ProcessIdentity,
    thread: &str,
    checkout: &Path,
    profile: Option<&Path>,
) -> Result<AgentRecord> {
    let Response::Agents { agents, .. } = backend
        .call(Request::List {
            all: false,
            project: Some(checkout.display().to_string()),
            labels: Default::default(),
        })
        .await
        .map_err(TransportError)?
    else {
        bail!("cannot list this checkout's agents");
    };
    let mut mine = agents.into_iter().filter(|agent| {
        agent.spec.runtime == "codex"
            && agent.status.is_live()
            && agent.pid == Some(terminal.pid)
            && agent.process_started_at == Some(terminal.started_at)
    });
    let found = mine.next();
    ensure!(
        mine.next().is_none(),
        "Codex terminal has more than one record"
    );
    let spec = match found {
        Some(agent) => match agent
            .spec
            .labels
            .get("session_id")
            .filter(|s| !s.is_empty())
        {
            Some(session) if session == thread => return Ok(agent),
            Some(_) => bail!("Codex terminal is bound to another conversation"),
            None => {
                let mut spec = agent.spec.clone();
                spec.labels.insert("session_id".into(), thread.to_owned());
                if let Some(profile) = profile {
                    spec.labels.insert(
                        agentdocker_core::agent::PROFILE_LABEL.into(),
                        profile.display().to_string(),
                    );
                }
                // Joined as the terminal's own registration, not adopted
                // again: an adoption of a registered process is refused.
                spec.labels.remove("adopted");
                spec
            }
        },
        None => AgentSpec {
            name: format!("codex-{}", terminal.pid),
            runtime: "codex".into(),
            workdir: Some(checkout.to_owned()),
            labels: [
                ("via".to_owned(), "hook".to_owned()),
                ("session_id".to_owned(), thread.to_owned()),
            ]
            .into_iter()
            // With its verified profile, the terminal names the
            // conversation's key: a record kept by that key takes it in.
            .chain(profile.map(|profile| {
                (
                    agentdocker_core::agent::PROFILE_LABEL.to_owned(),
                    profile.display().to_string(),
                )
            }))
            .collect(),
            ..AgentSpec::default()
        },
    };
    let agent = match backend
        .call(Request::Register {
            spec,
            pid: Some(terminal.pid),
            // The terminal's own multiplexer is not readable from the shared
            // app-server this runs in.
            session: None,
        })
        .await
        .map_err(TransportError)?
    {
        Response::Agent { agent } => agent,
        Response::Error { message, .. } => bail!("Codex terminal registration refused: {message}"),
        _ => bail!("unexpected Codex terminal registration response"),
    };
    // The id may change: a conversation that ran before takes its terminal
    // back under the record that held it.
    ensure!(
        agent.status.is_live()
            && agent.spec.runtime == "codex"
            && agent.pid == Some(terminal.pid)
            && agent.process_started_at == Some(terminal.started_at)
            && agent.spec.labels.get("session_id").map(String::as_str) == Some(thread),
        "Codex terminal registration did not bind this conversation"
    );
    Ok(agent)
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

    #[test]
    fn native_identity_accepts_renamed_dedicated_host_only_with_binding_proof() {
        let pid = std::process::id();
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        for name in ["codex-x86_64-pc-windows-msvc.exe", "private-provider"] {
            let process = procinfo::Process {
                pid,
                ppid: 0,
                argv: vec![
                    name.into(),
                    "app-server".into(),
                    "--listen".into(),
                    "ws://127.0.0.1:12345".into(),
                ],
            };
            let context = Context::from_host(pid, cwd.clone(), std::slice::from_ref(&process))
                .unwrap()
                .expect("an explicitly selected native executable need not be named codex");
            assert!(context.profile.is_none());
            assert_eq!(context.host.started_at, procinfo::start_time(pid).unwrap());
            assert_eq!(
                context.executable,
                procinfo::executable_path_of(pid)
                    .unwrap()
                    .canonicalize()
                    .unwrap()
            );
            let (_, mut agent, meta) = fixture();
            agent.spec.workdir = Some(cwd.clone());
            let calls = std::cell::Cell::new(0);
            let error = context
                .select(
                    vec![agent.clone()],
                    &meta,
                    |_| true,
                    |_| {
                        calls.set(calls.get() + 1);
                        anyhow::bail!("unaccepted host");
                    },
                )
                .unwrap_err();
            assert_eq!(calls.get(), 1);
            assert_eq!(error.to_string(), "unaccepted host");
            assert_eq!(
                context
                    .select(vec![agent.clone()], &meta, |_| true, |_| Ok(()))
                    .unwrap()
                    .id,
                agent.id
            );
            for missing in ["app-server", "--listen"] {
                let mut other = process.clone();
                other.argv.retain(|arg| arg != missing);
                assert!(
                    Context::from_host(pid, cwd.clone(), &[other])
                        .unwrap()
                        .is_none()
                );
            }
        }
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
    fn a_terminal_conversation_is_found_by_the_one_terminal_that_resumed_it() {
        let project = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let (mut context, _, _) = fixture();
        context.cwd = project.path().canonicalize().unwrap();
        // What `ps` shows; 105's own arguments are one prompt that reads
        // like a resume once `ps` joins it.
        let lines = std::collections::BTreeMap::from([
            (
                99,
                "node /lib/@openai/codex/bin/codex.js resume root-thread",
            ),
            (100, "codex resume root-thread --yolo"),
            (101, "codex resume root-thread"),
            (102, "codex resume another-thread"),
            (103, "codex"),
            (104, "codex resume root-thread"),
            (105, "codex resume root-thread"),
        ]);
        let process = |pid: u32| procinfo::Process {
            pid,
            ppid: if pid == 100 { 99 } else { 1 },
            argv: lines[&pid].split(' ').map(str::to_owned).collect(),
        };
        let whole = |pid: u32| {
            Some(if pid == 105 {
                vec!["codex".to_owned(), "resume root-thread".to_owned()]
            } else {
                lines.get(&pid)?.split(' ').map(str::to_owned).collect()
            })
        };
        let born = Utc::now();
        let cwd = |pid| {
            Some(if pid == 101 {
                elsewhere.path().to_owned()
            } else {
                project.path().to_owned()
            })
        };
        let mut table: Vec<_> = [99, 100, 101, 102, 103, 105].map(process).into();
        let found = context
            .terminal(&table, "root-thread", cwd, whole, |_| Some(born))
            .unwrap();
        assert_eq!(
            (found.pid, found.started_at),
            (100, born),
            "not its launcher, another checkout's, another thread's or a prompt"
        );
        let none = context
            .terminal(&table, "child-thread", cwd, whole, |_| Some(born))
            .unwrap_err();
        assert!(unbound(&none), "{none:#}");
        // Unreadable arguments prove nothing.
        let unreadable = context
            .terminal(&table, "root-thread", cwd, |_| None, |_| Some(born))
            .unwrap_err();
        assert!(unbound(&unreadable));
        table.push(process(104));
        let twice = context
            .terminal(&table, "root-thread", cwd, whole, |_| Some(born))
            .unwrap_err();
        assert!(!unbound(&twice), "two terminals are refused, not unbound");
    }

    /// A shared Codex server also serves conversations started in a terminal,
    /// which read their own queue when a message does not fit the hook: the
    /// inbox tools stay listed, refused only for a bound conversation.
    #[tokio::test]
    async fn a_shared_codex_server_lists_the_inbox_tools_terminal_conversations_need() {
        use crate::{client::mock::Mock, mcp::McpServer};
        let (context, _, _) = fixture();
        let mut server = McpServer::new(Mock::with(vec![]), context.unbound_identity());
        server.native_context = Some(context);
        let listed = server
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
            .await
            .unwrap();
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for tool in ["read_inbox", "wait_for_messages", "acknowledge_messages"] {
            assert!(names.contains(&tool), "{tool} is listed");
        }
    }

    /// A conversation no binding names and no terminal proves attaches by
    /// its key (this profile, its root thread) through the shared cached
    /// host only. A binding that names it, live or not, refuses instead of
    /// opening a second way into its queue; a child thread asks nothing.
    #[tokio::test]
    async fn an_unbound_conversation_attaches_by_its_key_or_is_refused() {
        use crate::client::mock::Mock;
        let (context, bound, meta) = fixture();
        let listed = |agents: Vec<AgentRecord>| Response::Agents {
            agents,
            aliases: Default::default(),
        };
        let mut keyed = bound.clone();
        keyed.input_binding = None;
        keyed.pid = None;
        let backend = Mock::with(vec![
            listed(Vec::new()),
            Response::Agent {
                agent: keyed.clone(),
            },
        ]);
        let attached = context
            .attach_verified(&backend, &meta, agentdocker_core::AdapterKind::Mcp)
            .await
            .unwrap();
        assert_eq!(attached.id, keyed.id);
        assert!(matches!(
            &backend.requests()[1],
            Request::SessionAttach { runtime, profile, session, workdir, adapter }
                if runtime == "codex" && profile == "/profile" && session == "root-thread"
                    && workdir == Path::new("/project")
                    && *adapter == agentdocker_core::AdapterKind::Mcp
        ));

        // Its binding exists but cannot be used here: refused, never attached.
        let backend = Mock::with(vec![listed(vec![bound])]);
        let refused = context
            .attach_verified(&backend, &meta, agentdocker_core::AdapterKind::Hooks)
            .await
            .unwrap_err();
        assert!(!unbound(&refused) && refused.to_string().contains("recover it"));
        assert_eq!(backend.requests().len(), 1, "nothing attached");

        // A child thread is not its root conversation.
        let backend = Mock::with(vec![]);
        let child = json!({"threadId":"child-thread","sessionId":"root-thread"});
        assert!(
            context
                .attach_verified(&backend, &child, agentdocker_core::AdapterKind::Mcp)
                .await
                .is_err()
        );
        assert!(backend.requests().is_empty());

        // A dedicated server never names one.
        let (mut dedicated, _, meta) = fixture();
        dedicated.profile = None;
        let backend = Mock::with(vec![]);
        let refused = dedicated
            .attach(&backend, &meta, agentdocker_core::AdapterKind::Mcp)
            .await
            .unwrap_err();
        assert!(unbound(&refused));
        assert!(backend.requests().is_empty());
    }

    /// A dedicated server earns authority only through an accepted binding:
    /// a terminal is never found for it, and nothing is asked of the daemon.
    #[tokio::test]
    async fn a_dedicated_host_never_finds_a_terminal() {
        use crate::client::mock::Mock;
        let (mut context, _, meta) = fixture();
        context.profile = None;
        let backend = Mock::with(vec![]);
        let refused = context.resolve_terminal(&backend, &meta).await.unwrap_err();
        assert!(unbound(&refused));
        assert!(backend.requests().is_empty());
    }

    /// A terminal registered for its conversation names the verified
    /// profile too, so the daemon matches the conversation's key and a
    /// record kept by that key takes the terminal in.
    #[tokio::test]
    async fn a_terminal_names_its_profile_so_its_keyed_record_takes_it_in() {
        use crate::client::mock::Mock;
        let now = Utc::now();
        let terminal = ProcessIdentity {
            pid: 4243,
            started_at: now,
        };
        // The daemon answers with the keyed record, the terminal joined.
        let (_, mut keyed, _) = fixture();
        keyed.input_binding = None;
        keyed.status = agentdocker_core::AgentStatus::Running;
        keyed.pid = Some(terminal.pid);
        keyed.process_started_at = Some(now);
        keyed
            .spec
            .labels
            .insert("session_id".into(), "root-thread".into());
        let backend = Mock::with(vec![
            Response::Agents {
                agents: vec![],
                aliases: Default::default(),
            },
            Response::Agent { agent: keyed },
        ]);
        bind_terminal(
            &backend,
            &terminal,
            "root-thread",
            Path::new("/project"),
            Some(Path::new("/profile")),
        )
        .await
        .unwrap();
        assert!(matches!(
            &backend.requests()[1],
            Request::Register { spec, pid: Some(4243), .. }
                if spec.labels.get(agentdocker_core::agent::PROFILE_LABEL).map(String::as_str)
                    == Some("/profile")
                    && spec.labels.get("session_id").map(String::as_str) == Some("root-thread")
        ));
    }

    #[tokio::test]
    async fn a_terminal_record_learns_its_conversation_or_is_registered() {
        use crate::client::mock::Mock;
        let now = Utc::now();
        let terminal = ProcessIdentity {
            pid: 4243,
            started_at: now,
        };
        let checkout = Path::new("/project");
        let record = |name: &str, session: Option<&str>| {
            let mut agent = AgentRecord::new(
                AgentSpec {
                    name: name.into(),
                    runtime: "codex".into(),
                    workdir: Some(checkout.to_owned()),
                    ..AgentSpec::default()
                },
                false,
                now,
            );
            agent.pid = Some(terminal.pid);
            agent.process_started_at = Some(now);
            if let Some(session) = session {
                agent
                    .spec
                    .labels
                    .insert("session_id".into(), session.into());
            }
            agent
        };
        let listed = |agents: Vec<AgentRecord>| Response::Agents {
            agents,
            aliases: Default::default(),
        };

        // A record without its conversation learns it; the daemon folds it
        // into the record that held the conversation before.
        let folded = record("codex-4244", Some("root-thread"));
        let backend = Mock::with(vec![
            listed(vec![record("codex-sample", None)]),
            Response::Agent {
                agent: folded.clone(),
            },
        ]);
        let bound = bind_terminal(&backend, &terminal, "root-thread", checkout, None)
            .await
            .unwrap();
        assert_eq!(bound.id, folded.id);
        assert!(matches!(
            &backend.requests()[1],
            Request::Register { spec, pid: Some(4243), session: None }
                if spec.name == "codex-sample"
                    && spec.labels.get("session_id").map(String::as_str) == Some("root-thread")
        ));

        // An adopted terminal joins as its own registration, not adopted
        // again, which the daemon would refuse for a registered process.
        let mut adopted = record("codex-sample", None);
        adopted.spec.labels.insert("adopted".into(), "true".into());
        let backend = Mock::with(vec![
            listed(vec![adopted]),
            Response::Agent {
                agent: folded.clone(),
            },
        ]);
        bind_terminal(&backend, &terminal, "root-thread", checkout, None)
            .await
            .unwrap();
        assert!(matches!(
            &backend.requests()[1],
            Request::Register { spec, .. } if !spec.labels.contains_key("adopted")
        ));

        // Already bound: nothing is registered.
        let backend = Mock::with(vec![listed(vec![record(
            "codex-sample",
            Some("root-thread"),
        )])]);
        bind_terminal(&backend, &terminal, "root-thread", checkout, None)
            .await
            .unwrap();
        assert_eq!(backend.requests().len(), 1);

        // No record yet: one is registered for the terminal.
        let backend = Mock::with(vec![
            listed(vec![]),
            Response::Agent {
                agent: record("codex-4243", Some("root-thread")),
            },
        ]);
        bind_terminal(&backend, &terminal, "root-thread", checkout, None)
            .await
            .unwrap();
        assert!(matches!(
            &backend.requests()[1],
            Request::Register { spec, .. } if spec.name == "codex-4243"
        ));

        // Bound to another conversation, or answered with a record that is
        // not this terminal's: refused.
        let backend = Mock::with(vec![listed(vec![record("codex-sample", Some("other"))])]);
        assert!(
            bind_terminal(&backend, &terminal, "root-thread", checkout, None)
                .await
                .is_err()
        );
        let mut someone_else = record("codex-1", Some("root-thread"));
        someone_else.pid = Some(1);
        let backend = Mock::with(vec![
            listed(vec![]),
            Response::Agent {
                agent: someone_else,
            },
        ]);
        assert!(
            bind_terminal(&backend, &terminal, "root-thread", checkout, None)
                .await
                .is_err()
        );
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

    /// What the remote connector takes for a Codex conversation: the
    /// shapes the native path reads, current and legacy, and nothing that
    /// another client might also send.
    #[test]
    fn a_codex_conversation_is_named_by_its_thread_or_turn_metadata() {
        for codex in [
            json!({"threadId":"root-thread","sessionId":"root-thread"}),
            json!({"threadId":"child-thread"}),
            json!({"x-codex-turn-metadata":{"thread_id":"root-thread","session_id":"root-thread"}}),
            json!({"x-codex-turn-metadata":{"session_id":"root-thread"}}),
        ] {
            assert!(names_codex_conversation(&codex), "{codex}");
        }
        for other in [
            Value::Null,
            json!({}),
            json!({"sessionId":"a-session"}),
            json!({"threadId":""}),
            json!({"threadId":7}),
            json!({"x-codex-turn-metadata":{}}),
            json!({"openai/locale":"fr-FR","openai/subject":"s"}),
            json!({"claudecode/toolUseId":"toolu_1"}),
        ] {
            assert!(!names_codex_conversation(&other), "{other}");
        }
    }

    #[test]
    fn native_identity_accepts_legacy_root_metadata_without_ignoring_conflicts() {
        let (mut context, agent, _) = fixture();
        let legacy = json!({"threadId":"root-thread", "x-codex-turn-metadata":{
            "thread_id":"root-thread", "session_id":"root-thread"}});
        for profile in [Some(PathBuf::from("/profile")), None] {
            context.profile = profile;
            let calls = std::cell::Cell::new(0);
            let proof = |_: &AgentRecord| {
                calls.set(calls.get() + 1);
                Ok(())
            };
            assert_eq!(
                context
                    .select(vec![agent.clone()], &legacy, |_| true, proof)
                    .unwrap()
                    .id,
                agent.id
            );
            assert_eq!(calls.get(), usize::from(context.profile.is_none()));
            let mut modern = legacy.clone();
            modern["sessionId"] = json!("root-thread");
            assert!(
                context
                    .select(vec![agent.clone()], &modern, |_| true, proof)
                    .is_ok()
            );
            for invalid in [
                json!({"threadId":"root-thread", "x-codex-turn-metadata":null}),
                json!({"threadId":"root-thread", "x-codex-turn-metadata":{}}),
                json!({"threadId":"root-thread", "x-codex-turn-metadata":{"session_id":"root-thread"}}),
                json!({"threadId":"root-thread", "x-codex-turn-metadata":{"thread_id":"root-thread"}}),
                json!({"threadId":"child", "x-codex-turn-metadata":{"thread_id":"child", "session_id":"root-thread"}}),
                json!({"threadId":"root-thread", "sessionId":"root-thread", "x-codex-turn-metadata":{"thread_id":"other", "session_id":"root-thread"}}),
                json!({"threadId":"root-thread", "sessionId":"root-thread", "x-codex-turn-metadata":{"thread_id":"root-thread", "session_id":"other"}}),
            ] {
                assert!(
                    context
                        .select(vec![agent.clone()], &invalid, |_| true, proof)
                        .is_err(),
                    "{invalid}"
                );
            }
            for malformed in [
                Value::Null,
                json!(false),
                json!(42),
                json!({}),
                json!([]),
                json!("other"),
            ] {
                let mut invalid = legacy.clone();
                invalid["sessionId"] = malformed;
                assert!(
                    context
                        .select(vec![agent.clone()], &invalid, |_| true, proof)
                        .is_err()
                );
            }
            assert!(
                context
                    .select(vec![agent.clone()], &legacy, |_| false, proof)
                    .is_err()
            );
            assert!(
                context
                    .select(vec![agent.clone(), agent.clone()], &legacy, |_| true, proof)
                    .is_err()
            );
            if context.profile.is_none() {
                assert!(
                    context
                        .select(
                            vec![agent.clone()],
                            &legacy,
                            |_| true,
                            |_| anyhow::bail!("dedicated host proof failed")
                        )
                        .is_err()
                );
            }
        }
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

    /// Calls distinguish backend refusal, missing bindings and stale local hosts.
    #[cfg(unix)]
    #[tokio::test]
    async fn native_call_keeps_transport_and_identity_failures_distinct() {
        use crate::{client::Backend, mcp::McpServer};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Inventory {
            unavailable: bool,
            calls: AtomicUsize,
        }
        impl Backend for Inventory {
            async fn call(&self, request: Request) -> Result<Response> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                // A conversation no binding names is attached by its key;
                // this daemon refuses that, so the failure stays its own.
                if matches!(request, Request::SessionAttach { .. }) {
                    return Ok(Response::error(
                        agentdocker_core::ErrorCode::Invalid,
                        "fixture refuses keyed sessions",
                    ));
                }
                assert!(matches!(request, Request::List { .. }));
                if self.unavailable {
                    Err(anyhow::anyhow!("fixture connection refused")
                        .context("cannot read fixture inventory"))
                } else {
                    Ok(Response::Agents {
                        agents: vec![],
                        aliases: Default::default(),
                    })
                }
            }
        }
        for (unavailable, stale) in [(true, false), (false, false), (true, true)] {
            let (mut context, _, meta) = fixture();
            let pid = std::process::id();
            context.host = ProcessIdentity {
                pid,
                started_at: procinfo::start_time(pid).unwrap(),
            };
            context.executable = procinfo::executable_path().unwrap().canonicalize().unwrap();
            if stale {
                context.host.started_at += chrono::Duration::seconds(1);
            }
            let mut server = McpServer::new(
                Inventory {
                    unavailable,
                    calls: AtomicUsize::new(0),
                },
                context.unbound_identity(),
            );
            server.native_context = Some(context);
            let mut attribution = None;
            let (code, message) = server
                .call_tool(
                    json!({"name":"whoami", "arguments":{}, "_meta":meta}),
                    &mut attribution,
                )
                .await
                .unwrap_err();
            assert_eq!(code, super::super::INTERNAL_ERROR);
            assert!(attribution.is_none(), "unverified requests stay unmeasured");
            // Stale: nothing asked. Unavailable: the first inventory read
            // fails. Missing binding: the inventory twice, then the attach.
            let expected = match (unavailable, stale) {
                (_, true) => 0,
                (true, false) => 1,
                (false, false) => 3,
            };
            assert_eq!(server.backend.calls.load(Ordering::SeqCst), expected);
            if unavailable && !stale {
                assert_eq!(
                    message,
                    "agentd unreachable: cannot read fixture inventory: fixture connection refused"
                );
            } else {
                assert!(
                    message.starts_with("cannot resolve native Codex identity: "),
                    "{message}"
                );
                assert!(!message.contains("agentd unreachable"));
                if stale {
                    assert!(message.contains("detached Codex MCP host identity changed"));
                }
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shared_native_batch_records_only_each_verified_conversation_after_output() {
        use crate::{client::Backend, mcp::McpServer};
        use agentdocker_core::usage::emission::Emission;
        use std::{cell::RefCell, collections::BTreeMap};

        struct Inventory {
            agents: Vec<AgentRecord>,
            emissions: RefCell<Vec<(String, Emission)>>,
        }
        impl Backend for Inventory {
            async fn call(&self, request: Request) -> Result<Response> {
                Ok(match request {
                    Request::List { .. } => Response::Agents {
                        agents: self.agents.clone(),
                        aliases: Default::default(),
                    },
                    Request::ReportAdapter { .. } => Response::Ok,
                    Request::Inspect { agent } => Response::Agent {
                        agent: self
                            .agents
                            .iter()
                            .find(|a| a.id.as_str() == agent)
                            .unwrap()
                            .clone(),
                    },
                    other => panic!("unexpected native coordination mutation: {other:?}"),
                })
            }
            async fn record_emission(&self, agent: &str, emission: Emission) {
                self.emissions
                    .borrow_mut()
                    .push((agent.to_owned(), emission));
            }
        }

        let (mut context, mut agent, meta) = fixture();
        let pid = std::process::id();
        context.host = ProcessIdentity {
            pid,
            started_at: procinfo::start_time(pid).unwrap(),
        };
        context.executable = procinfo::executable_path().unwrap().canonicalize().unwrap();
        agent.pid = Some(pid);
        agent.process_started_at = Some(context.host.started_at);
        agent.input_binding.as_mut().unwrap().provider.process = context.host.clone();
        let first = agent.id.to_string();
        let mut other = agent.clone();
        other.id = "second-native-root".to_owned().into();
        other.spec.name = "second-root".into();
        other.input_binding.as_mut().unwrap().provider.session = "other-root".into();
        let second = other.id.to_string();
        let mut server = McpServer::new(
            Inventory {
                agents: vec![agent, other],
                emissions: RefCell::new(Vec::new()),
            },
            context.unbound_identity(),
        );
        server.native_context = Some(context);
        let call = |id: u64, name: &str, meta: Value| {
            json!({
                "jsonrpc":"2.0", "id":id, "method":"tools/call",
                "params":{"name":name,"arguments":{"emission_agent":"forged"},"_meta":meta}
            })
        };
        let output = server
            .incoming_output(json!([
                {"jsonrpc":"2.0","id":0,"method":"initialize"},
                call(1, "whoami", meta.clone()),
                call(2, "whoami", json!({"threadId":"other-root","sessionId":"other-root"})),
                call(3, "whoami", json!({"threadId":"child","sessionId":"root-thread"})),
                call(4, "acknowledge_messages", meta),
                {"jsonrpc":"2.0","method":"notifications/initialized"}
            ]))
            .await
            .unwrap();
        assert!(
            server.backend.emissions.borrow().is_empty(),
            "no output yet"
        );
        assert!(
            server.identity.id.is_empty(),
            "shared host cannot retain a caller"
        );
        let mut wire = Vec::new();
        output.write(&server.backend, &mut wire).await.unwrap();
        let replies: Vec<Value> = serde_json::from_slice(&wire).unwrap();
        assert_eq!(replies.len(), 5);
        assert!(replies[3].get("error").is_some());
        assert!(replies[4].get("error").is_some());
        let sizes: BTreeMap<_, _> = server
            .backend
            .emissions
            .borrow()
            .iter()
            .map(|(agent, emission)| (agent.clone(), emission.bytes))
            .collect();
        assert_eq!(sizes.len(), 2);
        assert_eq!(
            sizes[&first],
            (serde_json::to_vec(&replies[1]).unwrap().len()
                + serde_json::to_vec(&replies[4]).unwrap().len()) as u64
        );
        assert_eq!(
            sizes[&second],
            serde_json::to_vec(&replies[2]).unwrap().len() as u64
        );
        let unknown = server
            .incoming_output(json!({"jsonrpc":"2.0","id":5,"method":"ping"}))
            .await
            .unwrap();
        unknown.write(&server.backend, &mut wire).await.unwrap();
        assert_eq!(
            server.backend.emissions.borrow().len(),
            2,
            "next request inherits nothing"
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
        let mut attribution = None;
        let result = server
            .call_tool(
                json!({"name":"ask_human", "arguments":{"question":"Fixture?"}, "_meta":meta}),
                &mut attribution,
            )
            .await
            .unwrap();
        let value: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(value["answer_delivery"], "native_queue");
        assert_eq!(attribution.as_deref(), Some(agent.id.as_str()));
        assert!(
            server.identity.id.is_empty(),
            "shared transport stays unbound"
        );
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

    /// A shared host refuses a conversation the tools its roles do not
    /// offer, and keeps that reading across calls, though each call binds
    /// the conversation afresh.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_shared_host_keeps_each_conversations_role_limit_across_calls() {
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
        let mut holder = agent.clone();
        holder.project = Some(agentdocker_core::ProjectRef::directory("/work/alpha"));
        holder.spec.labels.insert(
            agentdocker_core::agent::ROLE_LABEL.into(),
            "reviewer".into(),
        );
        let mut palette = agentdocker_core::role::defaults(
            &agentdocker_core::ProjectRef::directory("/work/alpha").id(),
            chrono::Utc::now(),
        );
        palette[2].tools = Some(vec!["get_task".into()]);
        let resolved = || Response::Agents {
            agents: vec![agent.clone()],
            aliases: Default::default(),
        };
        let backend = Mock::with(vec![
            resolved(),
            Response::Ok,
            Response::Agent { agent: holder },
            Response::RoleCatalog { roles: palette },
            resolved(),
            Response::Ok,
        ]);
        let mut server = McpServer::new(backend, context.unbound_identity()).limit_by_roles();
        server.native_context = Some(context);
        for _ in 0..2 {
            let refused = server
                .call_tool(
                    json!({"name":"claim", "arguments":{"resource":"path:/work/alpha"}, "_meta":meta}),
                    &mut None,
                )
                .await
                .unwrap_err();
            assert!(refused.1.contains("its roles (reviewer)"), "{}", refused.1);
        }
        let requests = server.backend.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|r| matches!(r, Request::Inspect { .. } | Request::RoleCatalog { .. }))
                .count(),
            2,
            "read once for both calls: {requests:?}"
        );
        assert!(!requests.iter().any(|r| matches!(r, Request::Claim { .. })));
    }
}
