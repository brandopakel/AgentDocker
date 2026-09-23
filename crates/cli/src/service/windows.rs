//! Per-user Windows startup through Task Scheduler. The definition carries an
//! ownership nonce and exact action; an unrelated task is never replaced.

use super::{DaemonCommand, Layout};
use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const RECORD_FORMAT: u32 = 1;
const RECORD_LIMIT: u64 = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Definition {
    task: String,
    home: PathBuf,
    description: String,
    executable: PathBuf,
    arguments: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    format: u32,
    current: Definition,
    // A prepared update accepts either exact definition after interruption.
    previous: Option<Definition>,
}

fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn encoded(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn task_name(home: &Path) -> String {
    let hash = Sha256::digest(home.as_os_str().as_encoded_bytes());
    format!("AgentDocker-{:x}", hash)
}

fn powershell() -> Result<PathBuf> {
    let root = std::env::var_os("SystemRoot").context("Windows SystemRoot is not set")?;
    let executable = PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if !executable.is_absolute() || !executable.is_file() {
        bail!("cannot find Windows PowerShell in SystemRoot");
    }
    Ok(executable)
}

fn matches_definition(definition: &Definition) -> String {
    format!(
        "($task.Description -ceq {} -and @($task.Actions).Count -eq 1 -and $task.Actions[0].Execute -ieq {} -and $task.Actions[0].Arguments -ceq {})",
        quoted(&definition.description),
        quoted(&definition.executable.to_string_lossy()),
        quoted(&definition.arguments),
    )
}

fn ownership_guard(receipt: Option<&Receipt>) -> String {
    let Some(receipt) = receipt else {
        return "if($null -ne $task){throw 'An existing task has no AgentDocker ownership record; it was preserved.'}".into();
    };
    let current = matches_definition(&receipt.current);
    let accepted = receipt
        .previous
        .as_ref()
        .map_or(current.clone(), |previous| {
            format!("({current} -or {})", matches_definition(previous))
        });
    format!(
        "if($null -ne $task){{if(-not {accepted}){{throw 'The scheduled task action or ownership was changed; it was preserved.'}}; $principal=[Security.Principal.NTAccount]::new($task.Principal.UserId); try{{$owner=$principal.Translate([Security.Principal.SecurityIdentifier]).Value}}catch{{$owner=$task.Principal.UserId}}; if($owner -ne $sid -or $task.Principal.LogonType.ToString() -ne 'Interactive' -or $task.Principal.RunLevel.ToString() -ne 'Limited'){{throw 'The scheduled task principal does not match this user; it was preserved.'}}}}"
    )
}

fn receipt_path(layout: &Layout) -> PathBuf {
    layout.home.join("windows-service.json")
}

fn read_receipt(layout: &Layout) -> Result<Option<Receipt>> {
    use std::io::Read;
    let file = match agentdocker_host::dirs::read_private_file(&receipt_path(layout)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read Windows service ownership"),
    };
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RECORD_LIMIT {
        bail!("Windows service ownership record exceeds its limit");
    }
    let receipt: Receipt = serde_json::from_slice(&bytes)
        .context("Windows service ownership record is invalid; no task was changed")?;
    if receipt.format != RECORD_FORMAT {
        bail!("unknown Windows service ownership format; no task was changed");
    }
    for definition in std::iter::once(&receipt.current).chain(receipt.previous.as_ref()) {
        if definition.home != layout.home || definition.task != task_name(&layout.home) {
            bail!("Windows service ownership names a different home; no task was changed");
        }
    }
    Ok(Some(receipt))
}

fn write_receipt(layout: &Layout, receipt: &Receipt) -> Result<()> {
    use std::io::Write;
    // Recheck an existing record before replacing it; protected creation plus
    // atomic publication cannot leave a partly written receipt on interruption.
    let _ = read_receipt(layout)?;
    let staged = layout
        .home
        .join(format!(".windows-service-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = agentdocker_host::dirs::create_private_file(&staged)?;
    let result = (|| -> Result<()> {
        let bytes = serde_json::to_vec(receipt)?;
        if bytes.len() as u64 > RECORD_LIMIT {
            bail!("Windows service definition exceeds its ownership-record limit");
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        agentdocker_host::files::publish_staged(&staged, &receipt_path(layout))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    result
}

fn evaluate(layout: &Layout, script: &str) -> Result<String> {
    let script = format!(
        "$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue';[Console]::OutputEncoding=[Text.UTF8Encoding]::new();{script}"
    );
    let argv = vec![
        powershell()?.to_string_lossy().into_owned(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-EncodedCommand".into(),
        encoded(&script),
    ];
    let output = agentdocker_host::command::run(
        &layout.user_home,
        &argv,
        std::time::Duration::from_secs(20),
    )?;
    if !output.success {
        bail!("Windows service operation failed: {}", output.text.trim());
    }
    Ok(output.stdout.trim().to_owned())
}

fn task_context(layout: &Layout, receipt: Option<&Receipt>) -> String {
    format!(
        "$taskPath='\\';$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;$task=Get-ScheduledTask -TaskPath $taskPath -TaskName {} -ErrorAction SilentlyContinue;{};",
        quoted(&task_name(&layout.home)),
        ownership_guard(receipt),
    )
}

fn desired(layout: &Layout, owner: Option<&Receipt>) -> Result<Definition> {
    let controller = agentdocker_host::procinfo::executable_path()?;
    let endpoint = layout
        .socket
        .clone()
        .unwrap_or_else(|| agentdocker_host::dirs::socket_path(&layout.home));
    let script = format!(
        "& {} daemon supervise --home {} --agentd {} --endpoint {}; exit $LASTEXITCODE",
        quoted(&controller.to_string_lossy()),
        quoted(&layout.home.to_string_lossy()),
        quoted(&layout.agentd.to_string_lossy()),
        quoted(&endpoint.to_string_lossy()),
    );
    Ok(Definition {
        task: task_name(&layout.home),
        home: layout.home.clone(),
        description: owner.map_or_else(
            || {
                format!(
                    "AgentDocker per-user daemon; ownership {}",
                    uuid::Uuid::new_v4()
                )
            },
            |owner| owner.current.description.clone(),
        ),
        executable: powershell()?,
        arguments: format!(
            "-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}",
            encoded(&script)
        ),
    })
}

fn install_script(layout: &Layout, receipt: &Receipt) -> String {
    let definition = &receipt.current;
    format!(
        "{} $action=New-ScheduledTaskAction -Execute {} -Argument {}; $trigger=New-ScheduledTaskTrigger -AtLogOn -User $sid; $principal=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited; $settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew; Register-ScheduledTask -TaskPath $taskPath -TaskName {} -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description {} -Force | Out-Null; Start-ScheduledTask -TaskPath $taskPath -TaskName {};",
        task_context(layout, Some(receipt)),
        quoted(&definition.executable.to_string_lossy()),
        quoted(&definition.arguments),
        quoted(&definition.task),
        quoted(&definition.description),
        quoted(&definition.task),
    )
}

fn stop_task_script(layout: &Layout, receipt: Option<&Receipt>) -> String {
    let task = quoted(&task_name(&layout.home));
    format!(
        "{} if($null -ne $task){{Stop-ScheduledTask -TaskPath $taskPath -TaskName {task}; $until=[DateTime]::UtcNow.AddSeconds(10); do{{$task=Get-ScheduledTask -TaskPath $taskPath -TaskName {task}; if($task.State.ToString() -ne 'Running'){{break}}; Start-Sleep -Milliseconds 100}}while([DateTime]::UtcNow -lt $until); if($task.State.ToString() -eq 'Running'){{throw 'The owned task did not stop within 10 seconds.'}}}}",
        task_context(layout, receipt),
    )
}

/// Only exact owned definitions may be started, stopped or removed. The
/// credential-free Interactive principal starts at this user's next login.
pub(super) async fn handle(
    layout: &Layout,
    client: &crate::client::Client,
    command: &DaemonCommand,
) -> Result<bool> {
    let dry_run = matches!(
        command,
        DaemonCommand::Install { dry_run: true } | DaemonCommand::Uninstall { dry_run: true }
    );
    let relevant = matches!(
        command,
        DaemonCommand::Install { .. }
            | DaemonCommand::Uninstall { .. }
            | DaemonCommand::Start
            | DaemonCommand::Stop
            | DaemonCommand::Restart
            | DaemonCommand::Status
    );
    if !relevant {
        return Ok(false);
    }
    let receipt = read_receipt(layout)?;
    if matches!(command, DaemonCommand::Status) {
        let state = evaluate(
            layout,
            &format!(
                "{} if($null -eq $task){{'not installed'}}else{{$task.State.ToString()}}",
                task_context(layout, receipt.as_ref()),
            ),
        )?;
        println!(
            "service   Task Scheduler, {state} ({})",
            task_name(&layout.home)
        );
        return Ok(false); // Common status code also describes the daemon.
    }
    if receipt.is_none()
        && !matches!(
            command,
            DaemonCommand::Install { .. } | DaemonCommand::Uninstall { .. }
        )
    {
        return Ok(false); // Ordinary on-demand start/stop still works.
    }
    if dry_run {
        match command {
            DaemonCommand::Install { .. } => {
                let desired = desired(layout, receipt.as_ref())?;
                println!(
                    "# Task Scheduler login task {} (current user, limited, no password)",
                    desired.task
                );
                println!(
                    "# daemon supervisor: {} --home {}",
                    layout.agentd.display(),
                    layout.home.display()
                );
                println!(
                    "# would write {} and start the task",
                    receipt_path(layout).display()
                );
            }
            DaemonCommand::Uninstall { .. } => println!(
                "# would stop and remove only the verified task {} and its ownership record",
                task_name(&layout.home)
            ),
            _ => unreachable!(),
        }
        return Ok(true);
    }
    if receipt.is_none() && matches!(command, DaemonCommand::Uninstall { .. }) {
        evaluate(layout, &task_context(layout, None))?;
        println!("the Windows daemon service is not installed");
        return Ok(true);
    }
    agentdocker_host::dirs::secure_state_dir(&layout.home)?;
    let _lock = agentdocker_host::lock::try_exclusive(&layout.home.join("windows-service.lock"))?
        .context("another Windows service operation is in progress")?;
    // A concurrent install may have completed before the lock was acquired.
    let receipt = read_receipt(layout)?;
    let selected = if let Some(receipt) = &receipt {
        evaluate(
            layout,
            &format!(
                "{} if($null -eq $task){{'absent'}}elseif({}){{'current'}}else{{'previous'}}",
                task_context(layout, Some(receipt)),
                matches_definition(&receipt.current),
            ),
        )?
    } else {
        evaluate(layout, &task_context(layout, None))?;
        "absent".into()
    };
    if !matches!(selected.as_str(), "absent" | "current" | "previous") {
        bail!("Windows task ownership query returned an unexpected result; no task was changed");
    }
    match command {
        DaemonCommand::Install { .. } => {
            let next = Receipt {
                format: RECORD_FORMAT,
                current: desired(layout, receipt.as_ref())?,
                previous: receipt
                    .as_ref()
                    .and_then(|receipt| match selected.as_str() {
                        "current" => Some(receipt.current.clone()),
                        "previous" => receipt.previous.clone(),
                        _ => None,
                    }),
            };
            super::retire(client).await?;
            evaluate(layout, &stop_task_script(layout, receipt.as_ref()))?;
            write_receipt(layout, &next)?;
            evaluate(layout, &install_script(layout, &next))?;
            write_receipt(
                layout,
                &Receipt {
                    previous: None,
                    ..next
                },
            )?;
            super::wait_for_daemon(client).await?;
        }
        DaemonCommand::Uninstall { .. } | DaemonCommand::Stop | DaemonCommand::Restart => {
            // A clean daemon shutdown deliberately stops managed sessions and
            // tells the supervisor to exit too. Stop the task after that.
            super::retire(client).await?;
            let context = task_context(layout, receipt.as_ref());
            let task = quoted(&task_name(&layout.home));
            evaluate(layout, &stop_task_script(layout, receipt.as_ref()))?;
            if matches!(command, DaemonCommand::Uninstall { .. }) {
                evaluate(
                    layout,
                    &format!(
                        "{context} if($null -ne $task){{Unregister-ScheduledTask -TaskPath $taskPath -TaskName {task} -Confirm:$false}}"
                    ),
                )?;
                if receipt.is_some() {
                    std::fs::remove_file(receipt_path(layout))?;
                }
            } else if matches!(command, DaemonCommand::Restart) {
                evaluate(
                    layout,
                    &format!(
                        "{context} if($null -eq $task){{throw 'The owned task is missing; run daemon install to restore it.'}}; Start-ScheduledTask -TaskPath $taskPath -TaskName {task}"
                    ),
                )?;
                super::wait_for_daemon(client).await?;
            }
        }
        DaemonCommand::Start => {
            evaluate(
                layout,
                &format!(
                    "{} if($null -eq $task){{throw 'The owned task is missing; run daemon install to restore it.'}}; Start-ScheduledTask -TaskPath $taskPath -TaskName {}",
                    task_context(layout, receipt.as_ref()),
                    quoted(&task_name(&layout.home))
                ),
            )?;
            super::wait_for_daemon(client).await?;
        }
        _ => unreachable!(),
    }
    Ok(true)
}

/// The scheduled action owns a small supervisor, while the daemon keeps the
/// same detached process semantics as on-demand startup. A clean shutdown is
/// deliberate; only failures restart, and repeated immediate failures stop.
pub(super) fn supervise(home: &Path, agentd: &Path, endpoint: &Path) -> Result<()> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    agentdocker_host::dirs::secure_state_dir(home)?;
    let _owner = agentdocker_host::lock::try_exclusive(&home.join("windows-supervisor.lock"))?
        .context("a daemon supervisor already owns this home")?;
    let mut restarts = 0;
    loop {
        let log = agentdocker_host::dirs::private_file(
            &agentdocker_core::paths::daemon_log(home),
            true,
            true,
        )?;
        let mut command = Command::new(agentd);
        agentdocker_host::command::detach(&mut command);
        command.args([
            "--home",
            &home.to_string_lossy(),
            "--socket",
            &endpoint.to_string_lossy(),
        ]);
        for key in [
            "AGENTDOCKER_AGENT_ID",
            "AGENTDOCKER_AGENT_NAME",
            "AGENTDOCKER_HOME",
            "AGENTDOCKER_SOCKET",
            "AGENTDOCKER_CLAUDE_CHANNEL_INPUT",
        ] {
            command.env_remove(key);
        }
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        let started = Instant::now();
        let status = command
            .spawn()
            .with_context(|| format!("cannot start {}", agentd.display()))?
            .wait()?;
        if status.success() {
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(600) {
            restarts = 0;
        }
        if restarts == 3 {
            bail!("agentd failed after three restart attempts ({status}); see the daemon log");
        }
        restarts += 1;
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Layout, Receipt) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let layout = Layout {
            agentd: home.join("agentd.exe"),
            home: home.clone(),
            socket: None,
            uid: 0,
            user_home: home,
        };
        let definition = Definition {
            task: task_name(&layout.home),
            home: layout.home.clone(),
            description: "AgentDocker per-user daemon; ownership fixture".into(),
            executable: PathBuf::from(
                "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
            ),
            arguments: "-NoProfile -EncodedCommand ZQB4AGkAdAAgADA=".into(),
        };
        let receipt = Receipt {
            format: RECORD_FORMAT,
            current: definition,
            previous: None,
        };
        (temp, layout, receipt)
    }

    #[test]
    fn interrupted_updates_retain_both_exact_actions_and_reject_foreign_records() {
        let (_temp, layout, mut receipt) = fixture();
        write_receipt(&layout, &receipt).unwrap();
        receipt.previous = Some(receipt.current.clone());
        receipt.current.arguments = "new action".into();
        write_receipt(&layout, &receipt).unwrap();
        let restored = read_receipt(&layout).unwrap().unwrap();
        let guard = ownership_guard(Some(&restored));
        assert!(guard.contains(&quoted("new action")));
        assert!(guard.contains(&quoted(&receipt.previous.as_ref().unwrap().arguments)));
        assert!(guard.contains("$owner -ne $sid"));
        assert!(guard.contains("'Interactive'"));
        assert!(guard.contains("'Limited'"));
        let mut other = layout.clone();
        other.home = layout.home.join("other");
        receipt.current.home = other.home;
        let bytes = serde_json::to_vec(&receipt).unwrap();
        std::fs::write(receipt_path(&layout), bytes).unwrap();
        assert!(
            read_receipt(&layout)
                .unwrap_err()
                .to_string()
                .contains("different home")
        );
        assert!(ownership_guard(None).contains("no AgentDocker ownership record"));
    }

    #[test]
    fn task_scripts_quote_paths_and_retain_literal_unicode_and_metacharacters() {
        let value = "C:\\space ü\\it's $(not code); & path";
        assert_eq!(quoted(value), "'C:\\space ü\\it''s $(not code); & path'");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded(value))
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), value);
        let (_temp, layout, mut receipt) = fixture();
        receipt.current.arguments = value.into();
        let script = install_script(&layout, &receipt);
        assert!(script.contains(&format!("-Argument {}", quoted(value))));
        assert!(script.contains("-LogonType Interactive -RunLevel Limited"));
        assert!(script.contains("-ExecutionTimeLimit ([TimeSpan]::Zero)"));
        assert!(!script.contains("-Password"));
        assert!(stop_task_script(&layout, Some(&receipt)).contains("AddSeconds(10)"));
    }

    #[test]
    fn invalid_or_oversized_receipts_are_never_adopted() {
        let (_temp, layout, mut receipt) = fixture();
        receipt.format += 1;
        let file = agentdocker_host::dirs::create_private_file(&receipt_path(&layout)).unwrap();
        serde_json::to_writer(&file, &receipt).unwrap();
        assert!(
            read_receipt(&layout)
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
        file.set_len(RECORD_LIMIT + 1).unwrap();
        assert!(
            read_receipt(&layout)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
    }
}
