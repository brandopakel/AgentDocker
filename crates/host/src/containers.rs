//! Container lifecycle I/O. CLI exit is never evidence that a container exited.
use crate::command;
use agentdocker_core::{AgentRecord, ContainerEngine, container::ManagedContainer};
use serde_json::Value;
use std::{path::Path, time::Duration};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ContainerError(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContainerState {
    Created,
    Running,
    Exited(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspection {
    pub id: String,
    pub state: ContainerState,
}

/// Synchronous operations run on blocking workers, outside daemon state locks.
pub trait ContainerBackend: Send + Sync {
    fn create(&self, record: &AgentRecord) -> Result<Inspection, ContainerError>;
    fn inspect(&self, record: &AgentRecord) -> Result<Inspection, ContainerError>;
    fn start(&self, record: &AgentRecord) -> Result<(), ContainerError>;
    fn stop(&self, record: &AgentRecord, force: bool) -> Result<(), ContainerError>;
    fn remove_created(&self, record: &AgentRecord) -> Result<(), ContainerError>;
    fn logs(&self, record: &AgentRecord, tail: usize) -> Result<String, ContainerError>;
}

pub struct CliContainers;

fn instance(record: &AgentRecord) -> &ManagedContainer {
    record.container.as_ref().expect("managed container record")
}

fn prefix(record: &AgentRecord) -> Vec<String> {
    let c = instance(record);
    let mut argv = vec![c.engine.to_string()];
    if let Some(connection) = &c.connection {
        argv.push(
            match c.engine {
                ContainerEngine::Docker => "--context",
                ContainerEngine::Podman => "--connection",
            }
            .into(),
        );
        argv.push(connection.clone());
    }
    argv
}

fn execute(record: &AgentRecord, args: Vec<String>) -> Result<command::Output, ContainerError> {
    let mut argv = prefix(record);
    argv.extend(args);
    let result = command::run(Path::new("/"), &argv, Duration::from_secs(15))
        .map_err(|e| ContainerError(e.to_string()))?;
    if !result.success {
        return Err(ContainerError(result.text.chars().take(2048).collect()));
    }
    Ok(result)
}

fn hash(raw: &str) -> Option<&str> {
    let raw = raw.strip_prefix("sha256:").unwrap_or(raw);
    (raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit())).then_some(raw)
}

/// Ownership, immutable image, ID, and exit state must all be explicit.
pub fn parse_inspection(record: &AgentRecord, raw: &str) -> Result<Inspection, ContainerError> {
    let data: Value = serde_json::from_str(raw).map_err(|e| ContainerError(e.to_string()))?;
    let data = data
        .as_array()
        .filter(|a| a.len() == 1)
        .and_then(|a| a.first())
        .ok_or_else(|| ContainerError("expected exactly one inspected container".into()))?;
    let c = instance(record);
    let id = data
        .get("Id")
        .and_then(Value::as_str)
        .and_then(hash)
        .ok_or_else(|| ContainerError("invalid container ID".into()))?;
    let label = |key: &str| data.get("Config")?.get("Labels")?.get(key)?.as_str();
    if c.id.as_deref().is_some_and(|expected| expected != id)
        || label("org.agentdocker.owner") != Some(c.owner.as_str())
        || label("org.agentdocker.agent") != Some(record.id.as_str())
        || label("org.agentdocker.build") != Some(c.build.as_str())
        || data.get("Image").and_then(Value::as_str).and_then(hash) != hash(&c.image_id)
        || hash(&c.image_id).is_none()
    {
        return Err(ContainerError(
            "container ownership or immutable image identity differs; protection retained".into(),
        ));
    }
    // Automatic engine restart/removal would make an exit observation unsafe.
    let policy = data
        .pointer("/HostConfig/RestartPolicy/Name")
        .and_then(Value::as_str);
    if !matches!(policy, Some("no" | ""))
        || data
            .pointer("/HostConfig/AutoRemove")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err(ContainerError(
            "container restart/removal policy is not managed by AgentDocker".into(),
        ));
    }
    let status = data.pointer("/State/Status").and_then(Value::as_str);
    let running = data.pointer("/State/Running").and_then(Value::as_bool);
    let restarting = data.pointer("/State/Restarting").and_then(Value::as_bool);
    let state = if running == Some(true) || restarting == Some(true) {
        ContainerState::Running
    } else if running == Some(false)
        && restarting == Some(false)
        && data.pointer("/State/Pid").and_then(Value::as_u64) == Some(0)
    {
        match status {
            Some("created" | "configured" | "initialized") => ContainerState::Created,
            Some("exited" | "stopped") => {
                let code = data
                    .pointer("/State/ExitCode")
                    .and_then(Value::as_i64)
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or_else(|| ContainerError("container exit code is missing".into()))?;
                ContainerState::Exited(code)
            }
            _ => {
                return Err(ContainerError(
                    "container exit is not confirmed; protection retained".into(),
                ));
            }
        }
    } else {
        return Err(ContainerError(
            "container liveness is uncertain; protection retained".into(),
        ));
    };
    Ok(Inspection {
        id: id.into(),
        state,
    })
}

fn create_args(record: &AgentRecord) -> Vec<String> {
    let c = instance(record);
    let mut args: Vec<String> = [
        "container",
        "create",
        "--pull=never",
        "--restart=no",
        "--network=none",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--name",
    ]
    .map(str::to_owned)
    .into();
    args.push(c.name.clone());
    for (key, value) in [
        ("owner", c.owner.as_str()),
        ("agent", record.id.as_str()),
        ("build", c.build.as_str()),
    ] {
        args.extend(["--label".into(), format!("org.agentdocker.{key}={value}")]);
    }
    for (key, value) in &record.spec.env {
        args.extend(["--env".into(), format!("{key}={value}")]);
    }
    args.extend([
        "--env".into(),
        format!("AGENTDOCKER_AGENT_ID={}", record.id),
        "--env".into(),
        format!("AGENTDOCKER_AGENT_NAME={}", record.spec.name),
        "--entrypoint".into(),
        record.spec.command[0].clone(),
        c.image_id.clone(),
    ]);
    args.extend(record.spec.command.iter().skip(1).cloned());
    args
}

impl ContainerBackend for CliContainers {
    fn create(&self, record: &AgentRecord) -> Result<Inspection, ContainerError> {
        let result = execute(record, create_args(record))?;
        let id = hash(result.stdout.trim())
            .ok_or_else(|| ContainerError("create returned an invalid container ID".into()))?;
        let mut confirmed = record.clone();
        confirmed.container.as_mut().unwrap().id = Some(id.into());
        self.inspect(&confirmed)
    }
    fn inspect(&self, record: &AgentRecord) -> Result<Inspection, ContainerError> {
        let c = instance(record);
        let target = c.id.as_ref().unwrap_or(&c.name);
        let output = execute(
            record,
            vec!["container".into(), "inspect".into(), target.clone()],
        )?;
        parse_inspection(record, &output.stdout)
    }
    fn start(&self, record: &AgentRecord) -> Result<(), ContainerError> {
        let inspected = self.inspect(record)?;
        if inspected.state != ContainerState::Created {
            return Err(ContainerError(
                "only a confirmed unstarted container can start".into(),
            ));
        }
        execute(
            record,
            vec!["container".into(), "start".into(), inspected.id],
        )?;
        Ok(())
    }
    fn stop(&self, record: &AgentRecord, force: bool) -> Result<(), ContainerError> {
        let inspected = self.inspect(record)?;
        if matches!(inspected.state, ContainerState::Exited(_)) {
            return Ok(());
        }
        let args = if force {
            vec![
                "container".into(),
                "kill".into(),
                "--signal=KILL".into(),
                inspected.id,
            ]
        } else {
            vec![
                "container".into(),
                "stop".into(),
                "--time=2".into(),
                inspected.id,
            ]
        };
        execute(record, args)?;
        Ok(())
    }
    fn remove_created(&self, record: &AgentRecord) -> Result<(), ContainerError> {
        let inspected = self.inspect(record)?;
        if inspected.state != ContainerState::Created {
            return Err(ContainerError(
                "refusing to remove a container which may have run".into(),
            ));
        }
        // No --force: a concurrent engine start must make removal fail.
        execute(record, vec!["container".into(), "rm".into(), inspected.id])?;
        Ok(())
    }
    fn logs(&self, record: &AgentRecord, tail: usize) -> Result<String, ContainerError> {
        let inspected = self.inspect(record)?;
        let output = execute(
            record,
            vec![
                "container".into(),
                "logs".into(),
                "--timestamps".into(),
                "--tail".into(),
                tail.min(10000).to_string(),
                inspected.id,
            ],
        )?;
        Ok(output.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdocker_core::{AgentSpec, container::ContainerIntent};
    use serde_json::json;

    fn record(engine: ContainerEngine) -> AgentRecord {
        let mut r = AgentRecord::new(
            AgentSpec {
                command: vec!["sh".into(), "-c".into(), "echo hello".into()],
                ..AgentSpec::default()
            },
            true,
            chrono::Utc::now(),
        );
        r.container = Some(ManagedContainer {
            engine,
            connection: Some("chosen".into()),
            build: "build".into(),
            image_id: format!("sha256:{}", "a".repeat(64)),
            name: "owned-name".into(),
            owner: "random-owner".into(),
            id: Some("b".repeat(64)),
            intent: ContainerIntent::Run,
            start_attempted: false,
            last_error: None,
        });
        r
    }
    fn inspect_json(record: &AgentRecord) -> Value {
        json!([{
            "Id": "b".repeat(64), "Image": instance(record).image_id,
            "Config": {"Labels": {"org.agentdocker.owner": "random-owner", "org.agentdocker.agent": record.id, "org.agentdocker.build":"build"}},
            "HostConfig": {"RestartPolicy":{"Name":"no"}, "AutoRemove":false},
            "State": {"Status":"exited", "Running":false, "Restarting":false, "Pid":0, "ExitCode":7}
        }])
    }
    #[test]
    fn inspection_requires_exact_identity_and_confirmed_exit() {
        let r = record(ContainerEngine::Docker);
        let raw = inspect_json(&r);
        assert_eq!(
            parse_inspection(&r, &raw.to_string()).unwrap().state,
            ContainerState::Exited(7)
        );
        for (path, value) in [
            ("/0/Id", json!("c".repeat(64))),
            ("/0/Image", json!(format!("sha256:{}", "c".repeat(64)))),
            ("/0/Config/Labels/org.agentdocker.owner", json!("other")),
            ("/0/Config/Labels/org.agentdocker.agent", json!("other")),
            ("/0/HostConfig/RestartPolicy/Name", json!("always")),
            ("/0/HostConfig/AutoRemove", json!(true)),
            ("/0/State/Running", Value::Null),
            ("/0/State/Pid", json!(123)),
            ("/0/State/Status", json!("removing")),
            ("/0/State/ExitCode", Value::Null),
        ] {
            let mut bad = raw.clone();
            *bad.pointer_mut(path).unwrap() = value;
            assert!(parse_inspection(&r, &bad.to_string()).is_err(), "{path}");
        }
        let mut running = raw.clone();
        running[0]["State"]["Running"] = json!(true);
        assert_eq!(
            parse_inspection(&r, &running.to_string()).unwrap().state,
            ContainerState::Running
        );
        assert!(parse_inspection(&r, "[]").is_err());
    }
    #[test]
    fn adapters_select_engine_and_create_without_automatic_restart_or_host_mounts() {
        for (engine, flag) in [
            (ContainerEngine::Docker, "--context"),
            (ContainerEngine::Podman, "--connection"),
        ] {
            let r = record(engine);
            assert_eq!(
                prefix(&r),
                [engine.to_string(), flag.into(), "chosen".into()]
            );
            let args = create_args(&r);
            assert!(args.iter().any(|a| a == "--pull=never"));
            assert!(args.iter().any(|a| a == "--restart=no"));
            assert!(args.iter().any(|a| a == "--network=none"));
            assert!(
                !args
                    .iter()
                    .any(|a| ["--rm", "--volume", "--mount", "--privileged"].contains(&a.as_str()))
            );
            assert!(args.windows(2).any(|a| a == ["--entrypoint", "sh"]));
            assert_eq!(
                &args[args.len() - 3..],
                [
                    instance(&r).image_id.clone(),
                    "-c".into(),
                    "echo hello".into()
                ]
            );
        }
    }
}
