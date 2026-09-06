//! Container lifecycle I/O. CLI exit is never evidence that a container exited.
use crate::command;
use agentdocker_core::{AgentRecord, ContainerEngine, container::ManagedContainer};
use serde_json::Value;
use std::{path::Path, time::Duration};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ContainerError {
    pub code: agentdocker_core::ErrorCode,
    pub message: String,
}
impl ContainerError {
    pub fn unavailable(message: String) -> Self {
        Self {
            code: agentdocker_core::ErrorCode::EngineUnavailable,
            message,
        }
    }
    pub fn invalid(message: String) -> Self {
        Self {
            code: agentdocker_core::ErrorCode::Invalid,
            message,
        }
    }
}

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
    if c.engine == ContainerEngine::Podman && c.connection.is_none() && c.workspace.is_some() {
        argv.push("--remote=false".into());
    }
    argv
}

fn execute(record: &AgentRecord, args: Vec<String>) -> Result<command::Output, ContainerError> {
    let mut argv = prefix(record);
    argv.extend(args);
    let result = command::run(Path::new("/"), &argv, Duration::from_secs(15))
        .map_err(|e| ContainerError::unavailable(e.to_string()))?;
    if !result.success {
        return Err(ContainerError::unavailable(
            result.text.chars().take(2048).collect(),
        ));
    }
    Ok(result)
}

fn hash(raw: &str) -> Option<&str> {
    let raw = raw.strip_prefix("sha256:").unwrap_or(raw);
    (raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit())).then_some(raw)
}

/// Ownership, immutable image, ID, and exit state must all be explicit.
pub fn parse_inspection(record: &AgentRecord, raw: &str) -> Result<Inspection, ContainerError> {
    let data: Value =
        serde_json::from_str(raw).map_err(|e| ContainerError::unavailable(e.to_string()))?;
    let data = data
        .as_array()
        .filter(|a| a.len() == 1)
        .and_then(|a| a.first())
        .ok_or_else(|| {
            ContainerError::unavailable("expected exactly one inspected container".into())
        })?;
    let c = instance(record);
    let id = data
        .get("Id")
        .and_then(Value::as_str)
        .and_then(hash)
        .ok_or_else(|| ContainerError::unavailable("invalid container ID".into()))?;
    let label = |key: &str| data.get("Config")?.get("Labels")?.get(key)?.as_str();
    if c.id.as_deref().is_some_and(|expected| expected != id)
        || label("org.agentdocker.owner") != Some(c.owner.as_str())
        || label("org.agentdocker.agent") != Some(record.id.as_str())
        || label("org.agentdocker.build") != Some(c.build.as_str())
        || data.get("Image").and_then(Value::as_str).and_then(hash) != hash(&c.image_id)
        || hash(&c.image_id).is_none()
    {
        return Err(ContainerError::unavailable(
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
        return Err(ContainerError::unavailable(
            "container restart/removal policy is not managed by AgentDocker".into(),
        ));
    }
    let status = data.pointer("/State/Status").and_then(Value::as_str);
    let running = data.pointer("/State/Running").and_then(Value::as_bool);
    let restarting = data.pointer("/State/Restarting").and_then(Value::as_bool);
    if let Some(w) = &c.workspace {
        if data.pointer("/Config/User").and_then(Value::as_str) != Some(&w.user)
            || data.pointer("/Config/WorkingDir").and_then(Value::as_str) != Some("/workspace")
            || data
                .pointer("/HostConfig/NetworkMode")
                .and_then(Value::as_str)
                != Some(c.options.network.as_str())
            || data.pointer("/HostConfig/Privileged") != Some(&Value::Bool(false))
        {
            return Err(ContainerError::unavailable(
                "container user, workdir, networking or privilege policy differs".into(),
            ));
        }
        let mounts = data
            .get("Mounts")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ContainerError::unavailable("container omitted mount evidence".into())
            })?;
        let mut expected = vec![(&w.checkout, "/workspace", !w.read_only)];
        if let Some(g) = &w.git {
            expected.push((&g.common, "/run/agentdocker-git", !w.read_only));
        }
        if let Some(a) = &w.access {
            expected.push((&a.directory, "/run/agentdocker-auth", false));
            if a.relay.is_none() {
                expected.push((&a.socket_directory, "/run/agentdocker", false));
            }
        }
        let relay = w.access.as_ref().and_then(|a| a.relay.as_ref());
        let volume_matches = relay.is_none_or(|r| {
            mounts.iter().any(|m| {
                m.get("Type").and_then(Value::as_str) == Some("volume")
                    && m.get("Name").and_then(Value::as_str) == Some(&r.volume)
                    && m.get("Destination").and_then(Value::as_str) == Some("/run/agentdocker")
                    && m.get("RW").and_then(Value::as_bool) == Some(false)
            })
        });
        if !volume_matches
            || mounts.len() != expected.len() + usize::from(relay.is_some())
            || expected.iter().any(|(source, dest, rw)| {
                !mounts.iter().any(|m| {
                    m.get("Type").and_then(Value::as_str) == Some("bind")
                        && m.get("Source").and_then(Value::as_str) == source.to_str()
                        && m.get("Destination").and_then(Value::as_str) == Some(*dest)
                        && m.get("RW").and_then(Value::as_bool) == Some(*rw)
                })
            })
        {
            return Err(ContainerError::unavailable(
                "container mounts differ from the authorized checkout and endpoint".into(),
            ));
        }
    }
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
                    .ok_or_else(|| {
                        ContainerError::unavailable("container exit code is missing".into())
                    })?;
                ContainerState::Exited(code)
            }
            _ => {
                return Err(ContainerError::unavailable(
                    "container exit is not confirmed; protection retained".into(),
                ));
            }
        }
    } else {
        return Err(ContainerError::unavailable(
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
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--name",
    ]
    .map(str::to_owned)
    .into();
    args.push(c.name.clone());
    args.push(format!("--network={}", c.options.network.as_str()));
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
    if let Some(w) = &c.workspace {
        // The scoped host/SSH socket is outside container SELinux process labels.
        // Keep UID isolation, private mounts, dropped capabilities and no-new-privileges.
        args.push("--security-opt=label=disable".into());
        args.extend([
            "--user".into(),
            w.user.clone(),
            "--workdir=/workspace".into(),
        ]);
        if w.keep_id {
            args.push("--userns=keep-id".into());
        }
        args.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst=/workspace{}",
                w.checkout.display(),
                if w.read_only { ",readonly" } else { "" }
            ),
        ]);
        if let Some(g) = &w.git {
            args.extend([
                "--mount".into(),
                format!(
                    "type=bind,src={},dst=/run/agentdocker-git{}",
                    g.common.display(),
                    if w.read_only { ",readonly" } else { "" }
                ),
            ]);
            let directory = Path::new("/run/agentdocker-git").join(&g.directory);
            args.extend([
                format!("--env=GIT_DIR={}", directory.display()),
                "--env=GIT_COMMON_DIR=/run/agentdocker-git".into(),
                "--env=GIT_WORK_TREE=/workspace".into(),
            ]);
        }
        if let Some(a) = &w.access {
            args.extend([
                "--mount".into(),
                format!(
                    "type=bind,src={},dst=/run/agentdocker-auth,readonly",
                    a.directory.display()
                ),
                "--mount".into(),
                match &a.relay {
                    Some(relay) => format!(
                        "type=volume,src={},dst=/run/agentdocker,readonly",
                        relay.volume
                    ),
                    None => format!(
                        "type=bind,src={},dst=/run/agentdocker,readonly",
                        a.socket_directory.display()
                    ),
                },
                "--env=AGENTDOCKER_SOCKET=/run/agentdocker/endpoint.sock".into(),
                "--env=AGENTDOCKER_TOKEN_FILE=/run/agentdocker-auth/token".into(),
            ]);
        }
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
        let id = hash(result.stdout.trim()).ok_or_else(|| {
            ContainerError::unavailable("create returned an invalid container ID".into())
        })?;
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
            return Err(ContainerError::unavailable(
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
            return Err(ContainerError::unavailable(
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
            inputs: None,
            engine,
            connection: Some("chosen".into()),
            build: "build".into(),
            image_id: format!("sha256:{}", "a".repeat(64)),
            name: "owned-name".into(),
            owner: "random-owner".into(),
            id: Some("b".repeat(64)),
            intent: ContainerIntent::Run,
            start_attempted: false,
            create_attempted: false,
            last_error: None,
            options: Default::default(),
            workspace: None,
            deadline: None,
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
    fn workspace_inspection_rejects_added_mounts_wrong_users_and_networks() {
        use agentdocker_core::container::{ContainerWorkspace, WorkspaceAccess};
        let mut record = record(ContainerEngine::Docker);
        record.container.as_mut().unwrap().workspace = Some(ContainerWorkspace {
            git: None,
            checkout: "/checkout".into(),
            user: "1000:1000".into(),
            keep_id: false,
            read_only: false,
            access: Some(WorkspaceAccess {
                relay: None,
                grant: "grant".into(),
                directory: "/private-auth".into(),
                socket_directory: "/scoped-socket".into(),
                vm: None,
            }),
        });
        let mut raw = inspect_json(&record);
        raw[0]["Config"]["User"] = json!("1000:1000");
        raw[0]["Config"]["WorkingDir"] = json!("/workspace");
        raw[0]["HostConfig"]["NetworkMode"] = json!("none");
        raw[0]["HostConfig"]["Privileged"] = json!(false);
        raw[0]["Mounts"] = json!([
            {"Type":"bind","Source":"/checkout","Destination":"/workspace","RW":true},
            {"Type":"bind","Source":"/private-auth","Destination":"/run/agentdocker-auth","RW":false},
            {"Type":"bind","Source":"/scoped-socket","Destination":"/run/agentdocker","RW":false},
        ]);
        assert!(parse_inspection(&record, &raw.to_string()).is_ok());
        for (path, value) in [
            ("/0/Config/User", json!("0")),
            ("/0/HostConfig/NetworkMode", json!("host")),
            ("/0/HostConfig/Privileged", json!(true)),
            ("/0/Mounts/0/Source", json!("/")),
            ("/0/Mounts/1/RW", json!(true)),
        ] {
            let mut bad = raw.clone();
            *bad.pointer_mut(path).unwrap() = value;
            assert!(
                parse_inspection(&record, &bad.to_string()).is_err(),
                "{path}"
            );
        }
        let mut extra_volume = raw.clone();
        extra_volume[0]["Mounts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"Type":"volume","Name":"unapproved","Destination":"/data","RW":true}));
        assert!(parse_inspection(&record, &extra_volume.to_string()).is_err());
        record
            .container
            .as_mut()
            .unwrap()
            .workspace
            .as_mut()
            .unwrap()
            .git = Some(agentdocker_core::container::GitMounts {
            directory: "worktrees/worker".into(),
            common: "/repo/.git".into(),
        });
        assert!(
            parse_inspection(&record, &raw.to_string()).is_err(),
            "Git metadata mount is required"
        );
        raw[0]["Mounts"].as_array_mut().unwrap().push(json!({"Type":"bind","Source":"/repo/.git","Destination":"/run/agentdocker-git","RW":true}));
        assert!(parse_inspection(&record, &raw.to_string()).is_ok());
        record
            .container
            .as_mut()
            .unwrap()
            .workspace
            .as_mut()
            .unwrap()
            .read_only = true;
        assert!(
            parse_inspection(&record, &raw.to_string()).is_err(),
            "validation cannot use writable Git or source"
        );
        raw[0]["Mounts"][0]["RW"] = json!(false);
        raw[0]["Mounts"][3]["RW"] = json!(false);
        assert!(parse_inspection(&record, &raw.to_string()).is_ok());
        let git_args = create_args(&record);
        assert!(
            git_args
                .iter()
                .any(|a| a == "--env=GIT_DIR=/run/agentdocker-git/worktrees/worker")
        );
        assert!(
            git_args
                .iter()
                .any(|a| a == "type=bind,src=/repo/.git,dst=/run/agentdocker-git,readonly")
        );
        raw[0]["Mounts"].as_array_mut().unwrap().push(
            json!({"Type":"bind","Source":"/engine.sock","Destination":"/engine.sock","RW":true}),
        );
        assert!(parse_inspection(&record, &raw.to_string()).is_err());
        let args = create_args(&record);
        assert!(
            args.iter()
                .any(|a| a == "type=bind,src=/private-auth,dst=/run/agentdocker-auth,readonly")
        );
        assert!(!args.iter().any(|a| a.contains("grant")));
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
