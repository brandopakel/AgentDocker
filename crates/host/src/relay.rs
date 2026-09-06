//! Engine-owned transport resources; never a second agent writer or an engine mount.
use crate::{
    command, engine,
    transport::{TransportError, storage_error},
};
use agentdocker_core::{
    AgentRecord, ContainerEngine, ErrorCode, ImageBuildSpec, container::EngineRelay,
};
use serde_json::Value;
use std::{fs, path::Path, time::Duration};

fn error(message: impl std::fmt::Display) -> TransportError {
    TransportError {
        code: ErrorCode::EngineUnavailable,
        message: message.to_string(),
    }
}
fn c(record: &AgentRecord) -> &agentdocker_core::container::ManagedContainer {
    record.container.as_ref().unwrap()
}
fn relay(record: &AgentRecord) -> &EngineRelay {
    c(record)
        .workspace
        .as_ref()
        .unwrap()
        .access
        .as_ref()
        .unwrap()
        .relay
        .as_ref()
        .unwrap()
}
pub fn prefix(record: &AgentRecord) -> Vec<String> {
    let c = c(record);
    let mut args = vec![c.engine.to_string()];
    if let Some(connection) = &c.connection {
        args.extend([
            if c.engine == ContainerEngine::Docker {
                "--context"
            } else {
                "--connection"
            }
            .into(),
            connection.clone(),
        ]);
    } else if c.engine == ContainerEngine::Podman {
        args.push("--remote=false".into());
    }
    args
}
fn run(record: &AgentRecord, args: Vec<String>) -> Result<String, TransportError> {
    let mut command = prefix(record);
    command.extend(args);
    let out = command::run(Path::new("/"), &command, Duration::from_secs(15)).map_err(error)?;
    if !out.success {
        return Err(error(out.text.chars().take(2048).collect::<String>()));
    }
    Ok(out.stdout)
}
fn json(record: &AgentRecord, args: Vec<String>) -> Result<Value, TransportError> {
    serde_json::from_str(&run(record, args)?).map_err(error)
}
fn hash(raw: &str) -> Option<&str> {
    let value = raw.strip_prefix("sha256:").unwrap_or(raw);
    (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())).then_some(value)
}

/// The pinned helper is built from captured embedded sources and retained with the run.
pub fn prepare(record: &mut AgentRecord) -> Result<(), TransportError> {
    let context = tempfile::tempdir().map_err(storage_error)?;
    fs::write(context.path().join("relay.py"), include_str!("relay.py")).map_err(storage_error)?;
    fs::write(context.path().join("Containerfile"),concat!(
        "FROM docker.io/library/python:3.13-alpine@sha256:7415fbc3c9e4979cc717d92377ab2bc7b2b4a2af1ac03cc52b5f3f88efedaf3a\n",
        "COPY relay.py /relay.py\nRUN mkdir -p /run/agentdocker && chmod 1777 /run/agentdocker\n",
        "ENTRYPOINT [\"python3\",\"-u\",\"/relay.py\"]\n")).map_err(storage_error)?;
    let build = engine::build(
        ImageBuildSpec {
            engine: c(record).engine,
            connection: c(record).connection.clone(),
            context: context.path().into(),
            recipe: "Containerfile".into(),
            timeout_secs: 600,
        },
        format!("relay-{}", record.id),
    )
    .map_err(|e| TransportError {
        code: ErrorCode::BuildFailed,
        message: e.to_string(),
    })?;
    // Prove engine-visible paths and effective UID without exposing a credential.
    let w = c(record).workspace.as_ref().unwrap();
    let mut roots = vec![&w.checkout, &w.access.as_ref().unwrap().directory];
    if let Some(g) = &w.git {
        roots.push(&g.common);
    }
    let mut probes = Vec::new();
    let mut args = vec![
        "container".into(),
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        "--network=none".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--security-opt=label=disable".into(),
        "--user".into(),
        w.user.clone(),
    ];
    if w.keep_id {
        args.push("--userns=keep-id".into());
    }
    for (i, root) in roots.iter().enumerate() {
        let probe = tempfile::Builder::new()
            .prefix(".agentdocker-probe-")
            .tempfile_in(root)
            .map_err(storage_error)?;
        fs::write(probe.path(), record.id.as_str()).map_err(storage_error)?;
        args.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst=/probe{i},readonly",
                probe.path().display()
            ),
        ]);
        probes.push(probe);
    }
    args.extend([
        "--entrypoint=python3".into(), build.image_id.clone(), "-c".into(),
        "from pathlib import Path; import sys; assert all(Path('/probe'+str(i)).read_text()==sys.argv[1] for i in range(int(sys.argv[2]))); print('shared')".into(),
        record.id.to_string(), probes.len().to_string(),
    ]);
    if run(record, args)?.trim() != "shared" {
        return Err(error(
            "engine cannot read the selected host paths with the workspace UID",
        ));
    }
    let transport = EngineRelay {
        name: format!("agentdocker-relay-{}", record.id),
        volume: format!("agentdocker-socket-{}", record.id),
        build,
        retired: false,
    };
    record
        .container
        .as_mut()
        .unwrap()
        .workspace
        .as_mut()
        .unwrap()
        .access
        .as_mut()
        .unwrap()
        .relay = Some(transport);
    Ok(())
}

fn volume(record: &AgentRecord) -> Result<(), TransportError> {
    let r = relay(record);
    let c = c(record);
    // Creation is idempotent by name; inspect labels even if an existing volume is returned.
    let _ = run(
        record,
        vec![
            "volume".into(),
            "create".into(),
            "--label".into(),
            format!("org.agentdocker.owner={}", c.owner),
            "--label".into(),
            format!("org.agentdocker.agent={}", record.id),
            r.volume.clone(),
        ],
    );
    // Podman reports already-exists; a lost create reply is also recoverable.
    // In every case inspect the resulting ownership before mounting anything.
    verify_volume(record)?;
    Ok(())
}
fn verify_volume(record: &AgentRecord) -> Result<(), TransportError> {
    let r = relay(record);
    let info = json(
        record,
        vec!["volume".into(), "inspect".into(), r.volume.clone()],
    )?;
    if info.pointer("/0/Name").and_then(Value::as_str) != Some(&r.volume)
        || info
            .pointer("/0/Labels/org.agentdocker.owner")
            .and_then(Value::as_str)
            != Some(&c(record).owner)
        || info
            .pointer("/0/Labels/org.agentdocker.agent")
            .and_then(Value::as_str)
            != Some(record.id.as_str())
        || info.pointer("/0/Driver").and_then(Value::as_str) != Some("local")
        || info
            .pointer("/0/Options")
            .is_some_and(|v| !v.is_null() && v.as_object().is_none_or(|o| !o.is_empty()))
    {
        return Err(error("engine socket volume ownership or driver differs"));
    }
    Ok(())
}
fn helper_id(record: &AgentRecord) -> Result<Option<String>, TransportError> {
    let r = relay(record);
    let found = run(
        record,
        vec![
            "container".into(),
            "ls".into(),
            "--all".into(),
            "--no-trunc".into(),
            "--filter".into(),
            format!("name={}", r.name),
            "--format={{.ID}}".into(),
        ],
    )?;
    if found.trim().is_empty() {
        return Ok(None);
    }
    hash(found.trim())
        .map(|id| Some(id.to_owned()))
        .ok_or_else(|| error("relay lookup did not return one full container identity"))
}
/// An owned relay can be replaced after a lost CLI stream; the writer and volume stay put.
fn remove_helper(record: &AgentRecord) -> Result<(), TransportError> {
    let r = relay(record);
    let expected_image =
        hash(&r.build.image_id).ok_or_else(|| error("invalid retained relay image ID"))?;
    let Some(id) = helper_id(record)? else {
        return Ok(());
    };
    let data = match json(
        record,
        vec!["container".into(), "inspect".into(), id.clone()],
    ) {
        Ok(data) => data,
        // Closing the attached stream can auto-remove the helper between list
        // and inspect. Require a successful new lookup proving absence; an
        // unavailable engine or an occupied name still leaves cleanup pending.
        Err(e) => {
            return if helper_id(record)?.is_none() {
                Ok(())
            } else {
                Err(e)
            };
        }
    };
    if data
        .pointer("/0/Name")
        .and_then(Value::as_str)
        .map(|s| s.trim_start_matches('/'))
        != Some(r.name.as_str())
        || data.pointer("/0/Id").and_then(Value::as_str).and_then(hash) != Some(id.as_str())
        || data
            .pointer("/0/Config/Labels/org.agentdocker.owner")
            .and_then(Value::as_str)
            != Some(&c(record).owner)
        || data
            .pointer("/0/Config/Labels/org.agentdocker.agent")
            .and_then(Value::as_str)
            != Some(record.id.as_str())
        || data
            .pointer("/0/Config/Labels/org.agentdocker.role")
            .and_then(Value::as_str)
            != Some("relay")
        || data
            .pointer("/0/Image")
            .and_then(Value::as_str)
            .and_then(hash)
            != Some(expected_image)
    {
        return Err(error(
            "refusing to remove a relay with different ownership or image",
        ));
    }
    match run(
        record,
        vec!["container".into(), "rm".into(), "--force".into(), id],
    ) {
        Ok(_) => Ok(()),
        Err(e) => {
            if helper_id(record)?.is_none() {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}
/// Confirm the helper's confinement before a writer receives its socket volume.
pub fn verify_helper(record: &AgentRecord) -> Result<(), TransportError> {
    let r = relay(record);
    let expected_image =
        hash(&r.build.image_id).ok_or_else(|| error("invalid retained relay image ID"))?;
    let w = c(record).workspace.as_ref().unwrap();
    let info = json(
        record,
        vec!["container".into(), "inspect".into(), r.name.clone()],
    )?;
    let data = info
        .as_array()
        .filter(|a| a.len() == 1)
        .and_then(|a| a.first())
        .ok_or_else(|| error("expected one relay container"))?;
    let mounts = data
        .get("Mounts")
        .and_then(Value::as_array)
        .ok_or_else(|| error("relay omitted mount evidence"))?;
    if data
        .get("Id")
        .and_then(Value::as_str)
        .and_then(hash)
        .is_none()
        || data
            .pointer("/Config/Labels/org.agentdocker.owner")
            .and_then(Value::as_str)
            != Some(&c(record).owner)
        || data
            .pointer("/Config/Labels/org.agentdocker.agent")
            .and_then(Value::as_str)
            != Some(record.id.as_str())
        || data
            .pointer("/Config/Labels/org.agentdocker.role")
            .and_then(Value::as_str)
            != Some("relay")
        || data.get("Image").and_then(Value::as_str).and_then(hash) != Some(expected_image)
        || data.pointer("/Config/User").and_then(Value::as_str) != Some(&w.user)
        || data
            .pointer("/HostConfig/NetworkMode")
            .and_then(Value::as_str)
            != Some("none")
        || data
            .pointer("/HostConfig/LogConfig/Type")
            .and_then(Value::as_str)
            != Some("none")
        || data.pointer("/HostConfig/Privileged") != Some(&Value::Bool(false))
        || !matches!(
            data.pointer("/HostConfig/RestartPolicy/Name")
                .and_then(Value::as_str),
            Some("no" | "")
        )
        || data.pointer("/State/Running") != Some(&Value::Bool(true))
        || mounts.len() != 1
        || mounts[0].get("Type").and_then(Value::as_str) != Some("volume")
        || mounts[0].get("Name").and_then(Value::as_str) != Some(&r.volume)
        || mounts[0].get("Destination").and_then(Value::as_str) != Some("/run/agentdocker")
        || mounts[0].get("RW") != Some(&Value::Bool(true))
    {
        return Err(error(
            "relay identity, mount, logging, user or network policy differs",
        ));
    }
    Ok(())
}

/// Return the attached CLI command only after ownership checks and stale-relay removal.
pub fn start_args(record: &AgentRecord) -> Result<Vec<String>, TransportError> {
    volume(record)?;
    remove_helper(record)?;
    let r = relay(record);
    let c = c(record);
    let w = c.workspace.as_ref().unwrap();
    let mut args = prefix(record);
    args.extend([
        "container".into(),
        "run".into(),
        "--rm".into(),
        "--interactive".into(),
        "--log-driver=none".into(),
        "--pull=never".into(),
        "--restart=no".into(),
        "--network=none".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--security-opt=label=disable".into(),
        "--memory=128m".into(),
        "--pids-limit=64".into(),
        "--name".into(),
        r.name.clone(),
        "--user".into(),
        w.user.clone(),
    ]);
    for (key, value) in [
        ("owner", c.owner.as_str()),
        ("agent", record.id.as_str()),
        ("role", "relay"),
    ] {
        args.extend(["--label".into(), format!("org.agentdocker.{key}={value}")]);
    }
    if w.keep_id {
        args.push("--userns=keep-id".into());
    }
    args.extend([
        "--mount".into(),
        format!("type=volume,src={},dst=/run/agentdocker", r.volume),
        r.build.image_id.clone(),
    ]);
    Ok(args)
}
pub fn cleanup(record: &AgentRecord) -> Result<(), TransportError> {
    remove_helper(record)?;
    // Exited agent containers are retained for logs and inspection. Their volume
    // stays with them until explicit engine cleanup; only the helper needs reaping.
    if c(record).create_attempted {
        return Ok(());
    }
    let r = relay(record);
    let volumes = run(
        record,
        vec![
            "volume".into(),
            "ls".into(),
            "--filter".into(),
            format!("name={}", r.volume),
            "--format={{.Name}}".into(),
        ],
    )?;
    if volumes.trim().is_empty() {
        return Ok(());
    }
    if volumes.trim() != r.volume {
        return Err(error("ambiguous socket volume identity"));
    }
    verify_volume(record)?;
    // Never force removal: the engine must confirm that no container still mounts it.
    run(record, vec!["volume".into(), "rm".into(), r.volume.clone()])?;
    Ok(())
}
