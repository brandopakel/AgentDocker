//! An owned connector login task, separate from the daemon's task and lifetime.
use super::Layout;
use crate::service::windows::{self as scheduler, Definition, Receipt};
use agentdocker_core::ProcessIdentity;
use agentdocker_host::{dirs, files, lock, procinfo};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn directory(home: &Path) -> PathBuf {
    home.join("connector")
}

fn task_name(home: &Path) -> String {
    format!(
        "AgentDocker-Connector-{:x}",
        Sha256::digest(home.as_os_str().as_encoded_bytes())
    )
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let file = match dirs::open_private_snapshot(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read connector service ownership"),
    };
    let mut bytes = Vec::new();
    file.take(scheduler::RECORD_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= scheduler::RECORD_LIMIT,
        "connector service record exceeds its size bound"
    );
    Ok(Some(serde_json::from_slice(&bytes).context(
        "invalid connector service record; no service was changed",
    )?))
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    match dirs::open_private_snapshot(path) {
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() as u64 <= scheduler::RECORD_LIMIT,
        "connector service record exceeds its size bound"
    );
    let mut staged = tempfile::Builder::new().make_in(
        path.parent().context("service record has no parent")?,
        dirs::create_private_file,
    )?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    files::publish_snapshot(&staged.into_temp_path(), path)?;
    Ok(())
}

fn receipt_path(home: &Path) -> PathBuf {
    directory(home).join("windows-service.json")
}

fn receipt(layout: &Layout) -> Result<Option<Receipt>> {
    let value: Option<Receipt> = read(&receipt_path(&layout.home))?;
    if let Some(value) = &value {
        ensure!(
            value.format == scheduler::RECORD_FORMAT,
            "unknown service record format"
        );
        for definition in std::iter::once(&value.current).chain(value.previous.as_ref()) {
            ensure!(
                definition.home == layout.home && definition.task == task_name(&layout.home),
                "connector service record names another home; no task was changed"
            );
        }
    }
    Ok(value)
}

fn desired(layout: &Layout, previous: Option<&Receipt>) -> Result<Definition> {
    let controller = crate::desktop::setup_executable()?;
    let owner = previous.map_or_else(
        || {
            format!(
                "AgentDocker per-user connector; ownership {}",
                uuid::Uuid::new_v4()
            )
        },
        |receipt| receipt.current.description.clone(),
    );
    let args = layout
        .serve_args
        .iter()
        .map(|arg| scheduler::quoted(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let endpoint = layout
        .socket
        .clone()
        .unwrap_or_else(|| dirs::socket_path(&layout.home));
    // The dependency is started by ServiceRun through Task Scheduler, never
    // as an on-demand child of this connector's task. Neither environment nor
    // the task definition carries an AgentDocker agent identity or credentials.
    // Windows PowerShell 5.1 turns redirected native stderr into error
    // records. Stop would abort the task on the connector's normal banner.
    // Keep setup strict, then supervise only failed native exits here. Task
    // Scheduler's RestartOnFailure did not recover the actual crash trial.
    // A clean service shutdown stays stopped; three failures are retried, with
    // a stable ten-minute run resetting the budget, as for the daemon service.
    let script = format!(
        "$ErrorActionPreference='Stop';$env:AGENTDOCKER_HOME={};$env:AGENTDOCKER_NO_AUTOSTART='1';Remove-Item Env:AGENTDOCKER_AGENT_ID,Env:AGENTDOCKER_AGENT_NAME,Env:AGENTDOCKER_SOCKET,Env:AGENTDOCKER_TOKEN_FILE -ErrorAction SilentlyContinue; $ErrorActionPreference='Continue'; $restarts=0; while($true){{$began=[DateTime]::UtcNow;$LASTEXITCODE=$null; & {} --socket {} connector service-run --owner {} {args} *> {}; $code=$LASTEXITCODE; if($null -eq $code){{exit 1}}; if($code -eq 0){{exit 0}}; if(([DateTime]::UtcNow-$began).TotalSeconds -ge 600){{$restarts=0}}; if($restarts -ge 3){{exit $code}}; $restarts++; Start-Sleep -Seconds 2}}",
        scheduler::quoted(&layout.home.to_string_lossy()),
        scheduler::quoted(&controller.to_string_lossy()),
        scheduler::quoted(&endpoint.to_string_lossy()),
        scheduler::quoted(&owner),
        scheduler::quoted(&layout.log().to_string_lossy()),
    );
    Ok(Definition {
        task: task_name(&layout.home),
        home: layout.home.clone(),
        description: owner,
        executable: scheduler::powershell()?,
        arguments: format!(
            "-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}",
            scheduler::encoded(&script)
        ),
    })
}

fn install_script(record: &Receipt, only_missing: bool) -> String {
    let value = &record.current;
    let register = format!(
        "$action=New-ScheduledTaskAction -Execute {} -Argument {}; $trigger=New-ScheduledTaskTrigger -AtLogOn -User $sid; $principal=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited; $settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew; Register-ScheduledTask -TaskPath $taskPath -TaskName {} -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description {} -Force | Out-Null;",
        scheduler::quoted(&value.executable.to_string_lossy()),
        scheduler::quoted(&value.arguments),
        scheduler::quoted(&value.task),
        scheduler::quoted(&value.description),
    );
    let register = if only_missing {
        format!("if($null -eq $task){{{register}}}")
    } else {
        register
    };
    format!(
        "{} {register} Start-ScheduledTask -TaskPath $taskPath -TaskName {};",
        scheduler::task_context_named(&value.task, Some(record)),
        scheduler::quoted(&value.task)
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Running {
    owner: String,
    nonce: String,
    process: ProcessIdentity,
}

impl Running {
    fn alive(&self) -> bool {
        procinfo::start_time(self.process.pid) == Some(self.process.started_at)
    }
}

fn stop_owned(layout: &Layout, record: Option<&Receipt>) -> Result<()> {
    scheduler::evaluate_at(
        &layout.user_home,
        &scheduler::task_context_named(&task_name(&layout.home), record),
    )?;
    let home = directory(&layout.home);
    if let Some(running) =
        read::<Running>(&home.join("windows-running.json"))?.filter(Running::alive)
    {
        ensure!(
            record.is_some_and(|record| {
                std::iter::once(&record.current)
                    .chain(record.previous.as_ref())
                    .any(|d| d.description == running.owner)
            }),
            "another connector owns this home; it was not stopped"
        );
        write(&home.join("windows-stop.json"), &running)?;
        let until = Instant::now() + Duration::from_secs(25);
        while running.alive() {
            ensure!(
                Instant::now() < until,
                "owned connector did not shut down; task and tunnel state preserved for inspection"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    } else if agentdocker_host::connector::serving(&layout.home).is_some() {
        bail!("a connector started outside this service is running; it was preserved");
    }
    scheduler::evaluate_at(
        &layout.user_home,
        &scheduler::stop_task_named(&task_name(&layout.home), record),
    )?;
    Ok(())
}

pub(super) fn install(layout: &Layout, dry_run: bool, enable: bool) -> Result<()> {
    let old = receipt(layout)?;
    let next = desired(layout, old.as_ref())?;
    if dry_run {
        println!(
            "# Windows connector login task {} (current user, limited, no password)\n# requires this home's owned daemon login task\n# would write {} and {}",
            next.task,
            receipt_path(&layout.home).display(),
            layout.log().display()
        );
        return Ok(());
    }
    let _registration = agentdocker_host::installation::guard_service_registration(
        std::slice::from_ref(&layout.agentdocker),
    )?;
    scheduler::connector_dependency(&layout.home, &layout.user_home, false)?;
    dirs::secure_state_dir(&directory(&layout.home))?;
    let _lock = lock::try_exclusive(&directory(&layout.home).join("windows-service.lock"))?
        .context("another connector service operation is in progress")?;
    let old = receipt(layout)?;
    let next = desired(layout, old.as_ref())?;
    if enable && let Some(old) = &old {
        ensure!(
            old.current == next && old.previous.is_none(),
            "a differently configured connector service exists; review it with `agentdocker connector install`; nothing was changed"
        );
    }
    let selected = scheduler::evaluate_at(
        &layout.user_home,
        &format!(
            "{} if($null -eq $task){{'absent'}}elseif({}){{'current'}}else{{'previous'}}",
            scheduler::task_context_named(&next.task, old.as_ref()),
            old.as_ref().map_or_else(
                || "$false".to_owned(),
                |record| scheduler::matches_definition(&record.current)
            )
        ),
    )?;
    ensure!(
        matches!(selected.as_str(), "absent" | "current" | "previous"),
        "unexpected task ownership response"
    );
    if !enable {
        stop_owned(layout, old.as_ref())?;
    } else if old.is_none() && agentdocker_host::connector::serving(&layout.home).is_some() {
        bail!("a connector started outside this service is running; it was preserved");
    }
    let pending = Receipt {
        format: scheduler::RECORD_FORMAT,
        current: next,
        previous: old.and_then(|record| match selected.as_str() {
            "current" => Some(record.current),
            "previous" => record.previous,
            _ => None,
        }),
    };
    // A running PowerShell task keeps the log open without sharing writes.
    // Identical enablement must leave that task and its output handle intact.
    // A new/replaced definition prepares its private log before it is started.
    if !enable || selected != "current" {
        dirs::private_file(&layout.log(), true, false)
            .context("cannot prepare the connector service log")?;
    }
    write(&receipt_path(&layout.home), &pending)?;
    scheduler::evaluate_at(&layout.user_home, &install_script(&pending, enable))?;
    write(
        &receipt_path(&layout.home),
        &Receipt {
            previous: None,
            ..pending
        },
    )?;
    eprintln!(
        "Connector login service enabled; `agentdocker connector status` reports readiness. Log: {}",
        layout.log().display()
    );
    Ok(())
}

pub(super) fn uninstall(layout: &Layout, dry_run: bool) -> Result<()> {
    if dry_run {
        println!(
            "# would stop and remove only the owned connector task {}",
            task_name(&layout.home)
        );
        return Ok(());
    }
    // Read-only cold absence still inspects foreign tasks; no state is created.
    let old = receipt(layout)?;
    if old.is_none() {
        scheduler::evaluate_at(
            &layout.user_home,
            &scheduler::task_context_named(&task_name(&layout.home), None),
        )?;
        eprintln!("the Windows connector service is not installed");
        return Ok(());
    }
    let _lock = lock::try_exclusive(&directory(&layout.home).join("windows-service.lock"))?
        .context("another connector service operation is in progress")?;
    let old = receipt(layout)?;
    stop_owned(layout, old.as_ref())?;
    scheduler::evaluate_at(
        &layout.user_home,
        &format!(
            "{} if($null -ne $task){{Unregister-ScheduledTask -TaskPath $taskPath -TaskName {} -Confirm:$false}}",
            scheduler::task_context_named(&task_name(&layout.home), old.as_ref()),
            scheduler::quoted(&task_name(&layout.home))
        ),
    )?;
    std::fs::remove_file(receipt_path(&layout.home))?;
    eprintln!("Connector login service removed; daemon and browser grants retained");
    Ok(())
}

#[derive(clap::Args, Debug)]
pub(crate) struct RunArgs {
    /// Ownership marker from this home's registered connector task.
    #[arg(long)]
    owner: String,
    #[command(flatten)]
    serve: super::super::ServeArgs,
}

async fn stopped(home: PathBuf, expected: Running) {
    loop {
        if read::<Running>(&directory(&home).join("windows-stop.json"))
            .ok()
            .flatten()
            .as_ref()
            == Some(&expected)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) async fn run(client: crate::client::Client, mut args: RunArgs) -> Result<()> {
    args.serve.service_socket = Some(client.socket_path().to_owned());
    let layout = super::layout(&args.serve)?;
    let root = directory(&layout.home);
    dirs::secure_state_dir(&root)?;
    // Installation starts the scheduled task while holding this same lock.
    // Wait for it to finish, then keep ownership validation and publication
    // atomic with respect to replacement/removal. Never wait on this lock
    // during shutdown: uninstall holds it while waiting for our exact exit.
    let until = tokio::time::Instant::now() + Duration::from_secs(30);
    let startup = loop {
        if let Some(held) = lock::try_exclusive(&root.join("windows-service.lock"))? {
            break held;
        }
        ensure!(
            tokio::time::Instant::now() < until,
            "connector service operation did not finish; startup was refused"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let record = receipt(&layout)?.context("connector service ownership is unavailable")?;
    let definition = desired(&layout, Some(&record))?;
    ensure!(
        std::iter::once(&record.current)
            .chain(record.previous.as_ref())
            .any(|owned| *owned == definition && owned.description == args.owner),
        "connector service arguments differ from the owned task"
    );
    let _lock = lock::try_exclusive(&root.join("windows-running.lock"))?
        .context("another connector service already owns this home")?;
    ensure!(
        !read::<Running>(&root.join("windows-running.json"))?.is_some_and(|r| r.alive()),
        "a live connector service record already exists"
    );
    ensure!(
        agentdocker_host::connector::serving(&layout.home).is_none(),
        "a connector started outside this service is running; it was preserved"
    );
    let pid = std::process::id();
    let running = Running {
        owner: args.owner,
        nonce: uuid::Uuid::new_v4().to_string(),
        process: ProcessIdentity {
            pid,
            started_at: procinfo::start_time(pid).context("connector process birth unavailable")?,
        },
    };
    write(&root.join("windows-running.json"), &running)?;
    drop(startup);
    let result = async {
        scheduler::connector_dependency(&layout.home, &layout.user_home, true)?;
        let client = client.with_start_timeout(None);
        let until = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if matches!(client.call(&agentdocker_core::Request::Ping).await, Ok(agentdocker_core::Response::Pong { .. })) {
                break;
            }
            ensure!(tokio::time::Instant::now() < until, "owned daemon login service did not become ready; connector did not start another daemon");
            tokio::select! {
                _ = stopped(layout.home.clone(), running.clone()) => return Ok(()),
                _ = tokio::time::sleep(Duration::from_millis(100)) => (),
            }
        }
        super::super::serve(
            client,
            args.serve,
            Some(Box::pin(stopped(layout.home.clone(), running.clone()))),
        )
        .await
    }
    .await;
    let mut cleanup_errors = Vec::new();
    for name in ["windows-running.json", "windows-stop.json"] {
        let path = root.join(name);
        let cleanup = (|| -> Result<()> {
            if read::<Running>(&path)?.as_ref() == Some(&running) {
                std::fs::remove_file(&path)?;
            }
            Ok(())
        })();
        if let Err(error) = cleanup {
            cleanup_errors.push(format!("{}: {error:#}", path.display()));
        }
    }
    if !cleanup_errors.is_empty() {
        let detail = format!(
            "connector service record cleanup failed: {}",
            cleanup_errors.join("; ")
        );
        return match result {
            Ok(()) => Err(anyhow::anyhow!(detail)),
            Err(error) => Err(error.context(detail)),
        };
    }
    result
}
