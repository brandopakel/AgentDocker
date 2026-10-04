//! Own a dedicated server and native TUI for zero-prompt input binding.
use super::{birth::Witness, bootstrap, ledger::Binding, remote};
use crate::{client::Client, codex_input::transport::birth::BirthObserver};
use agentdocker_core::{AgentSpec, ProcessIdentity, ProviderGeneration, Request, Response};
use agentdocker_host::{dirs, procinfo, provider_input};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    time::{Instant, timeout},
};

#[derive(clap::Args)]
pub struct Args {
    /// Actual native Codex executable, not an npm or shell wrapper.
    #[arg(long)]
    program: PathBuf,
    /// Provider profile; defaults to CODEX_HOME or ~/.codex. Never modified.
    #[arg(long)]
    profile: Option<PathBuf>,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long)]
    name: Option<String>,
    /// Reopen this exact persisted root conversation UUID without a new prompt.
    #[arg(long)]
    resume: Option<uuid::Uuid>,
    /// Provider configuration flags; initial prompts and resume are not accepted.
    #[arg(last = true, allow_hyphen_values = true)]
    arguments: Vec<String>,
}

fn identity(child: &Child) -> Result<ProcessIdentity> {
    let pid = child.id().context("owned native process has exited")?;
    Ok(ProcessIdentity {
        pid,
        started_at: procinfo::start_time(pid).context("owned native process birth unavailable")?,
    })
}

fn command(program: &Path, cwd: &Path, profile: &Path, home: &Path, client: &Client) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(cwd)
        .env("CODEX_HOME", profile)
        .env("AGENTDOCKER_HOME", home)
        .env("AGENTDOCKER_SOCKET", client.socket_path())
        .env_remove("AGENTDOCKER_AGENT_ID")
        .kill_on_drop(true);
    command
}

async fn retire(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let process = identity(child)?;
    if let Err(error) = procinfo::end(process.pid, process.started_at, false) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    match timeout(Duration::from_secs(5), child.wait()).await {
        Ok(result) => {
            result?;
            Ok(())
        }
        Err(_) => {
            child.start_kill()?;
            timeout(Duration::from_secs(5), child.wait())
                .await
                .context("owned native process did not exit")??;
            bail!("owned native process required forced shutdown");
        }
    }
}

async fn stop_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = term.recv() => (), _ = tokio::signal::ctrl_c() => () }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Drain stderr continuously while retaining at most 256 KiB of private evidence.
async fn log_output(mut input: tokio::process::ChildStderr, path: PathBuf) -> Result<()> {
    let mut file = dirs::create_private_file(&path)?;
    let mut buffer = [0; 8192];
    let mut retained = 0;
    loop {
        let n = input.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        let take = n.min((256 * 1024_usize).saturating_sub(retained));
        file.write_all(&buffer[..take])?;
        retained += take;
    }
    file.sync_all()?;
    Ok(())
}

async fn bind(client: &Client, binding: &Binding, directory: &Path) -> Result<Child> {
    let descriptor = bootstrap::launch(binding)?;
    let mut receiver = Command::new(&descriptor.executable);
    receiver
        .args(&descriptor.args)
        .envs(&descriptor.env)
        .env_remove("AGENTDOCKER_AGENT_ID")
        .env("AGENTDOCKER_HOME", dirs::home())
        .current_dir(&descriptor.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(dirs::create_private_file(
            &directory.join("receiver.log"),
        )?))
        .kill_on_drop(true);
    let mut child = receiver
        .spawn()
        .context("cannot start owned native receiver")?;
    let result = wait_binding(client, binding, Some(&mut child)).await;
    if let Err(error) = result {
        retire(&mut child).await?;
        return Err(error);
    }
    Ok(child)
}

async fn wait_binding(
    client: &Client,
    binding: &Binding,
    mut child: Option<&mut Child>,
) -> Result<()> {
    let descriptor = bootstrap::launch(binding)?;
    timeout(Duration::from_secs(30), async {
        loop {
            if let Some(child) = child.as_mut() {
                ensure!(
                    child.try_wait()?.is_none(),
                    "native receiver exited before binding"
                );
            }
            ensure!(
                super::alive(binding),
                "native terminal exited before binding"
            );
            if let Response::Agent { agent } = crate::codex_input::call(
                client,
                Request::Inspect {
                    agent: binding.agent.clone(),
                },
            )
            .await?
                && let Some(input) = agent.input_binding
            {
                ensure!(
                    input.provider == binding.provider,
                    "native receiver bound another provider generation"
                );
                ensure!(
                    input.launch.as_ref() == Some(&descriptor),
                    "native receiver bound another launch descriptor"
                );
                if procinfo::start_time(input.controller.pid) == Some(input.controller.started_at)
                    && child
                        .as_ref()
                        .is_none_or(|child| child.id() == Some(input.controller.pid))
                {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("native receiver binding timed out")
    .and_then(|result| result)
}

async fn wait_terminal(
    terminal: &mut Child,
    server: &mut Child,
    receiver: &mut Option<Child>,
) -> Result<()> {
    loop {
        tokio::select! {
            status = terminal.wait() => {
                ensure!(status?.success(), "native Codex terminal exited unsuccessfully");
                return Ok(());
            },
            _ = server.wait() => bail!("dedicated Codex server exited before its terminal"),
            status = async {
                match receiver.as_mut() {
                    Some(child) => child.wait().await,
                    None => std::future::pending().await,
                }
            } => {
                status.context("cannot reap native receiver")?;
                // The daemon owns replacement. Reap our original child promptly:
                // Linux otherwise keeps its zombie PID/birth visible as alive,
                // which prevents the daemon from starting a replacement.
                *receiver = None;
            },
        }
    }
}

struct Startup<'a> {
    client: &'a Client,
    cwd: &'a Path,
    profile: &'a Path,
    program: &'a Path,
    directory: &'a Path,
    port: u16,
    token: &'a str,
    name: Option<String>,
    resume: Option<&'a str>,
}

fn resumed_root(thread: &Value, session: &str, cwd: &Path) -> bool {
    thread["id"].as_str() == Some(session)
        && thread["sessionId"].as_str() == Some(session)
        && thread["cwd"]
            .as_str()
            .and_then(|p| Path::new(p).canonicalize().ok())
            .as_deref()
            == Some(cwd)
        && thread["threadSource"].as_str() == Some("user")
        && thread.get("forkedFromId").is_some_and(Value::is_null)
        && thread.get("parentThreadId").is_some_and(Value::is_null)
        && thread["ephemeral"].as_bool() == Some(false)
        && thread["status"]["type"].as_str() == Some("idle")
}

impl Startup<'_> {
    async fn witness(&self, server: &mut Child, tui: &mut Option<Child>) -> Result<Binding> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut observer = loop {
            ensure!(
                server.try_wait()?.is_none(),
                "owned Codex server exited during startup"
            );
            match BirthObserver::connect(self.port, self.token).await {
                Ok(observer) => break observer,
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        };
        // Inherit the physical terminal. No synthetic input or terminal proxy.
        let mut terminal = command(
            self.program,
            self.cwd,
            self.profile,
            &dirs::home(),
            self.client,
        );
        if let Some(session) = self.resume {
            terminal.args(["resume", session]);
        }
        *tui = Some(
            terminal
                .args([
                    "--no-alt-screen",
                    "--remote",
                    &format!("ws://127.0.0.1:{}", self.port),
                    "--remote-auth-token-env",
                    "AGENTDOCKER_NATIVE_CAPABILITY",
                ])
                .env("AGENTDOCKER_NATIVE_CAPABILITY", self.token)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .context("cannot start native Codex terminal")?,
        );
        let terminal = tui.as_mut().context("native terminal unavailable")?;
        let thread = timeout(Duration::from_secs(45), async {
            loop {
                ensure!(
                    server.try_wait()?.is_none() && terminal.try_wait()?.is_none(),
                    "owned native startup process exited"
                );
                let observed = match self.resume {
                    Some(session) => observer.resumed(session).await?,
                    None => observer.observed().await?,
                };
                if let Some(thread) = observed {
                    return Ok::<Value, anyhow::Error>(thread);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context("native terminal thread birth timed out")??;
        let session = thread["id"]
            .as_str()
            .context("native thread birth has no ID")?
            .to_owned();
        let provider = ProviderGeneration {
            process: identity(terminal)?,
            session: session.clone(),
            profile: self
                .profile
                .to_str()
                .context("provider profile is not UTF-8")?
                .into(),
        };
        let server = identity(server)?;
        let launcher = ProcessIdentity {
            pid: std::process::id(),
            started_at: procinfo::start_time(std::process::id())
                .context("native launcher birth unavailable")?,
        };
        let birth = if let Some(expected) = self.resume {
            ensure!(
                resumed_root(&thread, expected, self.cwd),
                "native terminal did not reopen the exact idle root conversation"
            );
            for process in [&provider.process, &server] {
                ensure!(
                    procinfo::inspect(process.pid).is_some_and(|p| p.ppid == launcher.pid),
                    "resumed native process is not owned by this launcher"
                );
            }
            // A resumed conversation must satisfy ordinary persisted history.
            // It can never receive the fresh thread's first-input allowance.
            None
        } else {
            let birth = Witness {
                launcher,
                created_at: thread["createdAt"]
                    .as_i64()
                    .context("native birth timestamp unavailable")?,
            };
            ensure!(
                birth.owns_children(&provider, &server)
                    && birth.matches_empty(
                        &thread,
                        &provider,
                        self.cwd,
                        chrono::Utc::now().timestamp()
                    ),
                "native birth lacks exact owned empty-thread proof"
            );
            Some(birth)
        };
        let spec = AgentSpec {
            name: self
                .name
                .clone()
                .unwrap_or_else(|| format!("codex-{}", provider.process.pid)),
            runtime: "codex".into(),
            workdir: Some(self.cwd.into()),
            labels: [("session_id".into(), session)].into(),
            ..Default::default()
        };
        let Response::Agent { agent } = crate::codex_input::call(
            self.client,
            Request::Register {
                spec,
                pid: Some(provider.process.pid),
                session: agentdocker_host::multiplexer::own(),
            },
        )
        .await?
        else {
            bail!("native terminal registration failed");
        };
        let mut binding = Binding {
            agent: agent.id.to_string(),
            provider,
            cwd: self.cwd.into(),
            executable: self.program.into(),
            socket: self.client.socket_path().into(),
            remote: None,
        };
        let record = remote::Record {
            version: if birth.is_some() { 2 } else { 1 },
            provider: binding.provider.clone(),
            server,
            executable: self.program.into(),
            cwd: self.cwd.into(),
            port: self.port,
            token_file: self.directory.join("capability"),
            token_sha256: format!("{:x}", Sha256::digest(self.token.as_bytes())),
            birth,
        };
        let path = self.directory.join("server.json");
        let mut file = dirs::create_private_file(&path)?;
        file.write_all(&serde_json::to_vec(&record)?)?;
        file.sync_all()?;
        binding.remote = Some(remote::describe(&path, &binding)?);
        observer.close().await?;
        Ok(binding)
    }
}

pub async fn run(client: Client, args: Args) -> Result<()> {
    let program = args
        .program
        .canonicalize()
        .context("native Codex executable unavailable")?;
    ensure!(program.is_file(), "native Codex executable is not a file");
    let cwd = args
        .cwd
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    let profile = args
        .profile
        .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
        .or_else(|| std::env::home_dir().map(|home| home.join(".codex")))
        .context("Codex profile unavailable")?
        .canonicalize()?;
    let options = provider_input::codex_arguments(&args.arguments)?;
    crate::codex_input::call(&client, Request::Ping).await?;
    let home = dirs::home();
    let parent = home.join("codex-native");
    dirs::ensure_private_dir(&parent)?;
    let directory = parent.join(uuid::Uuid::new_v4().simple().to_string());
    dirs::ensure_private_dir(&directory)?;
    let directory = directory.canonicalize()?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    // Own cleanup before the first fallible write/spawn. A startup failure must
    // not leave the capability behind before the normal shutdown path exists.
    let capability_path = tempfile::TempPath::try_from_path(directory.join("capability"))?;
    let mut capability = dirs::create_private_file(&capability_path)?;
    capability.write_all(token.as_bytes())?;
    capability.sync_all()?;
    drop(capability);
    let port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port();
    let mut server = command(&program, &cwd, &profile, &home, &client)
        .arg("app-server")
        .args(options)
        .args([
            "--listen",
            &format!("ws://127.0.0.1:{port}"),
            "--ws-auth",
            "capability-token",
            "--ws-token-file",
        ])
        .arg(directory.join("capability"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot start dedicated Codex server")?;
    let log = tokio::spawn(log_output(
        server
            .stderr
            .take()
            .context("native server log unavailable")?,
        directory.join("server.log"),
    ));
    let mut tui = None;
    let mut receiver = None;
    let resume = args.resume.map(|id| id.hyphenated().to_string());
    let startup = Startup {
        client: &client,
        cwd: &cwd,
        profile: &profile,
        program: &program,
        directory: &directory,
        port,
        token: &token,
        name: args.name,
        resume: resume.as_deref(),
    };
    let result = tokio::select! {
        _ = stop_signal() => Ok(()),
        result = async {
            let mut binding = startup.witness(&mut server,&mut tui).await?;
            let prior = if resume.is_some() {
                super::preflight(&binding).await?;
                super::resume::predecessor(&client, &binding).await?
            } else {
                None
            };
            if let Some(prior) = prior {
                binding = super::resume::handoff(&client, binding, &prior).await?;
                // Handoff publishes the canonical descriptor. Only the daemon
                // starts its receiver, preserving the old queue and ledger.
                wait_binding(&client, &binding, None).await?;
            } else {
                receiver = Some(bind(&client,&binding,&directory).await?);
            }
            eprintln!("AgentDocker native input ready: {}",binding.agent);
            let terminal = tui.as_mut().context("native terminal unavailable")?;
            wait_terminal(terminal, &mut server, &mut receiver).await
        } => result,
    };
    let mut cleanup = Ok(());
    // Retire the TUI first so daemon recovery cannot restart its receiver.
    if let Some(child) = tui.as_mut() {
        cleanup = cleanup.and(retire(child).await);
    }
    if let Some(child) = receiver.as_mut() {
        cleanup = cleanup.and(retire(child).await);
    }
    cleanup = cleanup.and(retire(&mut server).await);
    cleanup = cleanup.and(
        timeout(Duration::from_secs(5), log)
            .await
            .context("native server log did not close")
            .and_then(|r| r.context("native server log task failed"))
            .and_then(|r| r),
    );
    // Retain bounded private diagnostics/record; revoke the dead server capability.
    cleanup = cleanup.and(capability_path.close().map_err(Into::into));
    result.and(cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopen_requires_the_exact_idle_persisted_root_in_its_original_checkout() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().canonicalize().unwrap();
        let thread = serde_json::json!({
            "id":"original", "sessionId":"original", "cwd":cwd,
            "threadSource":"user", "forkedFromId":null, "parentThreadId":null,
            "ephemeral":false, "status":{"type":"idle"},
            "createdAt":1, "preview":"prior conversation", "turns":[{"id":"old"}]
        });
        assert!(resumed_root(&thread, "original", &cwd));
        for (key, value) in [
            ("id", serde_json::json!("different")),
            ("sessionId", serde_json::json!("different")),
            ("cwd", serde_json::json!(cwd.join("other"))),
            ("threadSource", serde_json::json!("subagent")),
            ("forkedFromId", serde_json::json!("parent")),
            ("parentThreadId", serde_json::json!("parent")),
            ("ephemeral", serde_json::json!(true)),
            ("status", serde_json::json!({"type":"active"})),
        ] {
            let mut changed = thread.clone();
            changed[key] = value;
            assert!(!resumed_root(&changed, "original", &cwd), "{key}");
            changed.as_object_mut().unwrap().remove(key);
            assert!(!resumed_root(&changed, "original", &cwd), "missing {key}");
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn receiver_is_reaped_while_native_terminal_remains_running() {
        fn sleeping_child() -> Child {
            Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .unwrap()
        }
        let mut terminal = sleeping_child();
        let mut server = sleeping_child();
        let receiver = sleeping_child();
        let process = identity(&receiver).unwrap();
        let mut receiver = Some(receiver);
        procinfo::end(process.pid, process.started_at, false).unwrap();
        let observed = async {
            timeout(Duration::from_secs(5), async {
                while procinfo::start_time(process.pid) == Some(process.started_at) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("exited receiver remained a zombie until terminal exit");
        };
        tokio::select! {
            result = wait_terminal(&mut terminal, &mut server, &mut receiver) => {
                panic!("receiver exit must not end the native terminal: {result:?}");
            },
            _ = observed => (),
        }
        assert!(receiver.is_none());
        assert!(terminal.try_wait().unwrap().is_none());
        assert!(server.try_wait().unwrap().is_none());
        retire(&mut terminal).await.unwrap();
        retire(&mut server).await.unwrap();
    }
}
