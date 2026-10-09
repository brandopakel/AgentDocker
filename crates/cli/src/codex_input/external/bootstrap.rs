//! Start one detached receiver from a verified provider hook. Never block input.
use super::ledger::{Binding, directory};
use crate::client::Client;
use agentdocker_core::{AgentRecord, ControllerLaunch, ProcessIdentity, ProviderGeneration};
use agentdocker_host::{dirs, lock, procinfo};
use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

/// The same immutable, non-secret launch descriptor is used by the first hook
/// and by daemon recovery. Authentication and drafts stay in the original TUI.
pub(super) fn launch(binding: &Binding) -> Result<ControllerLaunch> {
    let path = |value: &std::path::Path| -> Result<String> {
        Ok(value
            .to_str()
            .context("native receiver path is not UTF-8")?
            .into())
    };
    let mut descriptor = ControllerLaunch {
        executable: procinfo::executable_path()?.canonicalize()?,
        args: vec![
            "--socket".into(),
            path(&binding.socket)?,
            "codex-queue".into(),
            "--agent".into(),
            binding.agent.clone(),
            "--pid".into(),
            binding.provider.process.pid.to_string(),
            "--started-at".into(),
            binding.provider.process.started_at.to_rfc3339(),
            "--thread".into(),
            binding.provider.session.clone(),
            "--profile".into(),
            binding.provider.profile.clone(),
            "--cwd".into(),
            path(&binding.cwd)?,
            "--program".into(),
            path(&binding.executable)?,
        ],
        cwd: binding.cwd.clone(),
        env: [("AGENTDOCKER_NO_AUTOSTART".into(), "1".into())].into(),
    };
    if let Some(remote) = &binding.remote {
        descriptor
            .args
            .extend(["--app-server-record".into(), path(&remote.record)?]);
    }
    ensure!(
        descriptor.valid(),
        "native receiver launch descriptor is invalid"
    );
    Ok(descriptor)
}

fn marker_running(previous: &Binding, current: &Binding, process: ProcessIdentity) -> Result<bool> {
    if procinfo::start_time(process.pid) != Some(process.started_at) {
        return Ok(false);
    }
    ensure!(
        previous == current,
        "native controller marker belongs to another live provider generation"
    );
    Ok(true)
}

pub async fn ensure_started(client: &Client, agent: &AgentRecord) -> Result<bool> {
    if agent.managed || agent.spec.runtime != "codex" {
        return Ok(false);
    }
    // Installing a CLI does not replace an older active daemon. Probe a new
    // read-only operation before suppressing its still-working hook delivery.
    let capability = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        client.call_raw(&agentdocker_core::Request::PeekInput {
            agent: agent.id.to_string(),
        }),
    )
    .await;
    match capability {
        Ok(Ok(agentdocker_core::Response::Messages { .. })) => (),
        Ok(Ok(agentdocker_core::Response::Error {
            code: agentdocker_core::ErrorCode::Invalid,
            message,
            ..
        })) if message.starts_with("malformed request: unknown variant `peek_input`") => {
            return Ok(false);
        }
        _ => anyhow::bail!("daemon input ownership capability is unavailable"),
    }
    // `[experimental] native_codex = false` leaves an existing session to
    // its hooks: messages at tool boundaries, nothing started for it. The
    // daemon is asked only once it has shown it owns input at all.
    if !super::super::experimental(client).await.native_codex {
        return Ok(false);
    }
    let pid = agent.pid.context("Codex provider PID is unavailable")?;
    let birth = agent
        .process_started_at
        .context("Codex provider birth is unavailable")?;
    ensure!(
        procinfo::start_time(pid) == Some(birth),
        "Codex provider generation changed before bootstrap"
    );
    let profile = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|p| p.join(".codex")))
        .context("Codex provider profile is unavailable")?
        .canonicalize()?;
    let binding = Binding {
        agent: agent.id.to_string(),
        provider: ProviderGeneration {
            process: ProcessIdentity {
                pid,
                started_at: birth,
            },
            session: agent
                .spec
                .labels
                .get("session_id")
                .context("Codex thread is unavailable")?
                .clone(),
            profile: profile
                .to_str()
                .context("Codex profile path is not UTF-8")?
                .into(),
        },
        socket: client.socket_path().to_owned(),
        cwd: agent
            .spec
            .workdir
            .as_ref()
            .context("Codex checkout is unavailable")?
            .canonicalize()?,
        executable: procinfo::executable_path_of(pid)?.canonicalize()?,
        remote: None,
    };
    let home = dirs::home();
    let predecessor = if agent.input_binding.is_none() {
        super::resume::predecessor(client, &binding).await?
    } else {
        None
    };
    let directory = directory(&home, &binding.agent)?;
    let lock_path = directory.join("bootstrap.lock");
    dirs::private_file(&lock_path, true, false)?;
    let Some(_bootstrap) = lock::try_exclusive_existing(&lock_path)? else {
        return Ok(agent.input_binding.is_some());
    };
    if let Some(bound) = &agent.input_binding {
        ensure!(
            bound.provider == binding.provider,
            "retained native binding names another provider generation"
        );
        // The daemon owns restart/backoff once a launch descriptor is registered.
        // A hook must not bypass its restart limit or compete with a pending child.
        if bound.launch.is_some()
            || procinfo::start_time(bound.controller.pid) == Some(bound.controller.started_at)
        {
            return Ok(true);
        }
    }
    // Covers the short spawn-to-bind interval; the receiver's lifetime lock
    // remains authoritative if concurrent hook processes race this marker.
    let marker = directory.join("controller.json");
    if let Ok(file) = dirs::read_private_file(&marker) {
        let mut data = Vec::new();
        file.take(4097).read_to_end(&mut data)?;
        ensure!(
            data.len() <= 4096,
            "native controller marker exceeds its size limit"
        );
        if let Ok((previous, process)) = serde_json::from_slice::<(Binding, ProcessIdentity)>(&data)
        {
            if marker_running(&previous, &binding, process)? {
                // A starting helper has not proved queue capability or bound
                // ownership. Let the daemon arbitrate a legacy read until it
                // actually does; a mere process launch is not input readiness.
                return Ok(false);
            }
        }
    }
    let log = dirs::private_file(&directory.join("controller.log"), true, false)?;
    // One bounded diagnostic stream per controller generation.
    log.set_len(0)?;
    let mut descriptor = launch(&binding)?;
    if let Some(predecessor) = predecessor {
        descriptor
            .args
            .extend(["--predecessor".into(), predecessor]);
    }
    let mut command = Command::new(&descriptor.executable);
    command
        .args(&descriptor.args)
        .current_dir(&descriptor.cwd)
        .envs(&descriptor.env)
        .env("AGENTDOCKER_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid uses no allocation or shared Rust state after fork.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    agentdocker_host::command::detach(&mut command);
    let child = command
        .spawn()
        .context("could not start native Codex queue receiver")?;
    let process = ProcessIdentity {
        pid: child.id(),
        started_at: procinfo::start_time(child.id())
            .context("native receiver process birth is unavailable")?,
    };
    let mut file = tempfile::Builder::new().make_in(&directory, dirs::create_private_file)?;
    file.write_all(&serde_json::to_vec(&(binding, process))?)?;
    file.as_file().sync_all()?;
    agentdocker_host::files::publish_staged(&file.into_temp_path(), &marker)?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_live_marker_can_block_a_new_bootstrap_generation() {
        let process = ProcessIdentity {
            pid: std::process::id(),
            started_at: procinfo::start_time(std::process::id()).unwrap(),
        };
        let current = Binding {
            agent: "agent".into(),
            provider: ProviderGeneration {
                process: process.clone(),
                session: "thread".into(),
                profile: "/profile".into(),
            },
            socket: "/socket".into(),
            cwd: "/checkout".into(),
            executable: "/codex".into(),
            remote: None,
        };
        assert!(marker_running(&current, &current, process.clone()).unwrap());
        let mut previous = current.clone();
        previous.provider.session = "previous-thread".into();
        assert!(marker_running(&previous, &current, process.clone()).is_err());
        let reused_pid = ProcessIdentity {
            started_at: process.started_at - chrono::Duration::seconds(1),
            ..process
        };
        assert!(!marker_running(&previous, &current, reused_pid).unwrap());
        // The stale marker is atomically replaced only after launch succeeds;
        // the private ledger still independently validates generation changes.
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn older_daemon_keeps_legacy_delivery_but_other_failures_do_not_claim_support() {
        use agentdocker_core::{AgentSpec, ErrorCode, Response};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        for (code, message, unsupported) in [
            (
                ErrorCode::Invalid,
                "malformed request: unknown variant `peek_input`, expected ping",
                true,
            ),
            (ErrorCode::Unavailable, "daemon is unavailable", false),
            (ErrorCode::Forbidden, "queue is restricted", false),
        ] {
            let home = tempfile::tempdir().unwrap();
            let socket = home.path().join("agentd.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let request: agentdocker_core::Request = serde_json::from_str(&line).unwrap();
                assert!(matches!(
                    request,
                    agentdocker_core::Request::PeekInput { .. }
                ));
                let mut reply = serde_json::to_vec(&Response::error(code, message)).unwrap();
                reply.push(b'\n');
                stream.get_mut().write_all(&reply).await.unwrap();
            });
            let client = Client::new(Some(socket)).with_start_timeout(None);
            let agent = AgentRecord::new(
                AgentSpec {
                    runtime: "codex".into(),
                    ..Default::default()
                },
                false,
                chrono::Utc::now(),
            );
            let result = ensure_started(&client, &agent).await;
            if unsupported {
                assert!(!result.unwrap());
            } else {
                assert!(result.is_err());
            }
            task.await.unwrap();
        }
    }

    /// A daemon whose file says `native_codex = false` gets no receiver
    /// started for an existing session: the hook asks it, after the
    /// daemon has shown it owns input, and stands down.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_switch_off_leaves_an_existing_session_to_its_hooks() {
        use agentdocker_core::{AgentSpec, Request, Response, config::ExperimentalConfig};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("agentd.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let request: Request = serde_json::from_str(&line).unwrap();
                let response = match &request {
                    Request::PeekInput { .. } => Response::Messages {
                        messages: Vec::new(),
                    },
                    Request::Ping => Response::Pong {
                        version: "test".into(),
                        uptime_secs: 1,
                        restricted: None,
                        pid: None,
                        executable: None,
                        experimental: Some(ExperimentalConfig {
                            native_codex: false,
                            ..ExperimentalConfig::default()
                        }),
                    },
                    other => panic!("unexpected request {other:?}"),
                };
                seen.push(std::mem::discriminant(&request));
                let mut reply = serde_json::to_vec(&response).unwrap();
                reply.push(b'\n');
                stream.get_mut().write_all(&reply).await.unwrap();
            }
            assert_eq!(
                seen,
                [
                    std::mem::discriminant(&Request::PeekInput {
                        agent: String::new()
                    }),
                    std::mem::discriminant(&Request::Ping)
                ],
                "the capability probe comes first, the switch second"
            );
        });
        let client = Client::new(Some(socket)).with_start_timeout(None);
        let agent = AgentRecord::new(
            AgentSpec {
                runtime: "codex".into(),
                ..Default::default()
            },
            false,
            chrono::Utc::now(),
        );
        assert!(!ensure_started(&client, &agent).await.unwrap());
        task.await.unwrap();
    }
}
