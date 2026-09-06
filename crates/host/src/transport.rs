//! Checked local bind mounts and a scoped Unix-socket forward into a Podman VM.
use crate::{command, containers::ContainerError};
use agentdocker_core::ErrorCode;
use agentdocker_core::{
    AgentRecord, ContainerEngine,
    container::{ContainerWorkspace, PodmanVm, WorkspaceAccess},
};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct TransportError {
    pub code: ErrorCode,
    pub message: String,
}
impl From<TransportError> for ContainerError {
    fn from(error: TransportError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}
fn error(e: impl std::fmt::Display) -> TransportError {
    TransportError {
        code: ErrorCode::Invalid,
        message: e.to_string(),
    }
}
pub fn storage_error(e: impl std::fmt::Display) -> TransportError {
    TransportError {
        code: ErrorCode::StorageUnavailable,
        message: e.to_string(),
    }
}
fn engine_error(e: impl std::fmt::Display) -> TransportError {
    TransportError {
        code: ErrorCode::EngineUnavailable,
        message: e.to_string(),
    }
}
fn run(args: Vec<String>) -> Result<String, TransportError> {
    let out = command::run(Path::new("/"), &args, Duration::from_secs(15)).map_err(engine_error)?;
    if !out.success {
        return Err(engine_error(
            out.text.chars().take(2048).collect::<String>(),
        ));
    }
    Ok(out.stdout)
}
fn json(args: Vec<String>) -> Result<Value, TransportError> {
    serde_json::from_str(&run(args)?).map_err(error)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, TransportError> {
    v.pointer(key)
        .and_then(Value::as_str)
        .ok_or_else(|| error(format!("engine omitted {key}")))
}
fn path_arg(path: &Path) -> Result<String, TransportError> {
    let p = path
        .to_str()
        .ok_or_else(|| error("mount paths must be UTF-8"))?;
    if !path.is_absolute() || p.contains([',', ':', '\n', '\r', '\0']) {
        return Err(error(
            "mount path must be absolute without comma, colon or control characters",
        ));
    }
    Ok(p.into())
}
/// Never widen an existing directory or follow a substituted symlink.
pub fn private_directory(path: &Path) -> Result<(), TransportError> {
    let _ = create_private_directory(path)?;
    Ok(())
}
fn create_private_directory(path: &Path) -> Result<Option<fs::Metadata>, TransportError> {
    let created = match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(storage_error(e)),
    };
    let m = private_metadata(path)?;
    Ok(created.then_some(m))
}
fn private_metadata(path: &Path) -> Result<fs::Metadata, TransportError> {
    let m = fs::symlink_metadata(path).map_err(storage_error)?;
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.permissions().mode() & 0o077 != 0 {
        return Err(storage_error(
            "transport directory must be private and owned by the daemon user",
        ));
    }
    Ok(m)
}

/// Own only paths this attempt created, until both host preparation stages succeed.
#[derive(Default)]
pub struct Preparation {
    created: Vec<(PathBuf, fs::Metadata)>,
}
impl Preparation {
    fn directory(&mut self, path: &Path) -> Result<(), TransportError> {
        if let Some(metadata) = create_private_directory(path)? {
            self.created.push((path.into(), metadata));
        }
        Ok(())
    }
    fn known_hosts(&mut self, path: &Path) -> Result<(), TransportError> {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => self
                .created
                .push((path.into(), file.metadata().map_err(storage_error)?)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(storage_error(e)),
        }
        Ok(())
    }
    pub fn commit(mut self) {
        self.created.clear();
    }
}
impl Drop for Preparation {
    fn drop(&mut self) {
        for (path, original) in self.created.iter().rev() {
            let Ok(current) = fs::symlink_metadata(path) else {
                continue;
            };
            if current.uid() != unsafe { libc::geteuid() }
                || current.mode() & 0o077 != 0
                || current.dev() != original.dev()
                || current.ino() != original.ino()
                || current.file_type() != original.file_type()
            {
                continue;
            }
            // Never recurse: preserve new contents and paths substituted since creation.
            let _ = if current.is_dir() {
                fs::remove_dir(path)
            } else {
                fs::remove_file(path)
            };
        }
    }
}

/// Resolve the actual engine endpoint and host-visible UID before granting access.
pub fn prepare(
    record: &mut AgentRecord,
    home: &Path,
    control: &Path,
    preparation: &mut Preparation,
) -> Result<(), TransportError> {
    let checkout = fs::canonicalize(
        record
            .spec
            .workdir
            .as_ref()
            .ok_or_else(|| error("checkout mount requires --workdir"))?,
    )
    .map_err(error)?;
    let home = fs::canonicalize(home).map_err(storage_error)?;
    let control = fs::canonicalize(control).map_err(storage_error)?;
    if !checkout.is_dir()
        || home.starts_with(&checkout)
        || checkout.starts_with(&home)
        || control.starts_with(&checkout)
    {
        return Err(error(
            "checkout cannot contain daemon state or the control socket",
        ));
    }
    path_arg(&checkout)?;
    let git = if checkout.join(".git").is_file() {
        let (directory, common) = crate::vcs::git_dirs(&checkout)
            .ok_or_else(|| error("cannot resolve linked checkout metadata"))?;
        let directory = fs::canonicalize(directory).map_err(error)?;
        let common = fs::canonicalize(common).map_err(error)?;
        for path in [&directory, &common] {
            path_arg(path)?;
            if !path.is_dir()
                || home.starts_with(path)
                || path.starts_with(&home)
                || control.starts_with(path)
            {
                return Err(error(
                    "Git metadata cannot expose daemon state or control socket",
                ));
            }
        }
        let directory = directory
            .strip_prefix(&common)
            .map_err(|_| {
                error("linked Git directory must be inside its common repository metadata")
            })?
            .to_path_buf();
        Some(agentdocker_core::container::GitMounts { directory, common })
    } else {
        None
    };

    let parent = agentdocker_core::paths::workspace_dir(&home);
    preparation.directory(&parent)?;
    let parent = fs::canonicalize(parent).map_err(storage_error)?;
    let directory = parent.join(record.id.as_str());
    preparation.directory(&directory)?;
    path_arg(&directory)?;
    if directory.join("endpoint.sock").as_os_str().len() > 103 {
        return Err(error("daemon home is too long for workspace Unix sockets"));
    }
    let bridge_directory = parent.join(format!("bridge-{}", record.id));
    preparation.directory(&bridge_directory)?;
    let c = record.container.as_mut().unwrap();
    let mut vm = None;
    let uid;
    let gid;
    let keep_id;
    if let Some(machine) = &c.options.podman_machine {
        if c.engine != ContainerEngine::Podman {
            return Err(error("VM transport requires Podman"));
        }
        if machine.is_empty() || machine.starts_with('-') {
            return Err(error("invalid Podman machine name"));
        }
        let machines = json(vec![
            "podman".into(),
            "machine".into(),
            "inspect".into(),
            machine.clone(),
        ])?;
        let m = machines
            .as_array()
            .filter(|a| a.len() == 1)
            .and_then(|a| a.first())
            .ok_or_else(|| error("expected one Podman machine"))?;
        if text(m, "/State")? != "running" || m.get("Rootful") != Some(&Value::Bool(false)) {
            return Err(error("start the selected rootless Podman machine first"));
        }
        let port = m
            .pointer("/SSHConfig/Port")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
            .ok_or_else(|| error("invalid VM SSH port"))?;
        let user = text(m, "/SSHConfig/RemoteUsername")?.to_owned();
        if user.is_empty()
            || !user
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(error("invalid VM SSH username"));
        }
        let identity = Path::new(text(m, "/SSHConfig/IdentityPath")?).to_path_buf();
        let connections = json(vec![
            "podman".into(),
            "system".into(),
            "connection".into(),
            "list".into(),
            "--format=json".into(),
        ])?;
        let chosen = connections
            .as_array()
            .and_then(|a| {
                a.iter().find(|v| match &c.connection {
                    Some(name) => v.get("Name").and_then(Value::as_str) == Some(name),
                    None => v.get("Default") == Some(&Value::Bool(true)),
                })
            })
            .ok_or_else(|| error("selected Podman connection is unavailable"))?;
        let uri = text(chosen, "/URI")?;
        let expected = format!("ssh://{user}@127.0.0.1:{port}/run/user/");
        let remote_uid = uri
            .strip_prefix(&expected)
            .and_then(|s| s.strip_suffix("/podman/podman.sock"))
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| error("build connection must point to the selected rootless VM"))?;
        if text(chosen, "/Identity")? != identity.to_string_lossy() {
            return Err(error("VM identity differs from the engine connection"));
        }
        c.connection = Some(text(chosen, "/Name")?.into());
        let config = PodmanVm {
            machine: machine.clone(),
            port,
            identity,
            user,
        };
        preparation.known_hosts(&bridge_directory.join("known_hosts"))?;
        let ids = ssh_run(&config, &bridge_directory, "id -u; id -g")?;
        let ids: Vec<_> = ids
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<Result<_, _>>()
            .map_err(error)?;
        if ids.len() != 2 || ids[0] != remote_uid || ids[0] == 0 {
            return Err(error("VM user mapping does not match the engine"));
        }
        uid = ids[0];
        gid = ids[1];
        keep_id = true;
        // Prove that the engine VM sees these exact host directories before mounting.
        let mut shared = vec![&directory, &checkout];
        if let Some(git) = &git {
            shared.push(&git.common);
        }
        for root in shared {
            let probe = tempfile::Builder::new()
                .prefix(".agentdocker-mount-")
                .tempfile_in(root)
                .map_err(storage_error)?;
            let marker = record.id.to_string();
            fs::write(probe.path(), marker.as_bytes()).map_err(storage_error)?;
            let remote = ssh_run(
                &config,
                &bridge_directory,
                &format!("cat -- {}", quote(&path_arg(probe.path())?)),
            )?;
            if remote != marker {
                return Err(error(
                    "checkout or credential directory is not shared with the VM",
                ));
            }
        }
        vm = Some(config);
    } else {
        if !cfg!(target_os = "linux") && c.engine != ContainerEngine::Docker {
            return Err(error(
                "macOS Podman checkout mounts require --podman-machine",
            ));
        }
        uid = unsafe { libc::geteuid() };
        gid = unsafe { libc::getegid() };
        match c.engine {
            ContainerEngine::Podman => {
                if c.connection.is_some()
                    || std::env::var_os("CONTAINER_HOST").is_some()
                    || std::env::var_os("CONTAINER_CONNECTION").is_some()
                {
                    return Err(error("Linux checkout transport requires local Podman"));
                }
                let info = json(vec!["podman".into(), "info".into(), "--format=json".into()])?;
                if info.pointer("/host/security/rootless") != Some(&Value::Bool(true)) {
                    return Err(error("checkout mounts require rootless Podman"));
                }
                keep_id = true;
            }
            ContainerEngine::Docker => {
                let context = match &c.connection {
                    Some(v) => v.clone(),
                    None => run(vec!["docker".into(), "context".into(), "show".into()])?
                        .trim()
                        .into(),
                };
                let contexts = json(vec![
                    "docker".into(),
                    "context".into(),
                    "inspect".into(),
                    context.clone(),
                ])?;
                if !text(&contexts, "/0/Endpoints/docker/Host")?.starts_with("unix://")
                    || std::env::var_os("DOCKER_HOST").is_some()
                {
                    return Err(error(
                        "checkout mounts require a local Docker Unix endpoint",
                    ));
                }
                let endpoint = text(&contexts, "/0/Endpoints/docker/Host")?
                    .strip_prefix("unix://")
                    .unwrap();
                let endpoint = fs::canonicalize(endpoint).map_err(error)?;
                if endpoint.starts_with(&checkout)
                    || git
                        .as_ref()
                        .is_some_and(|g| endpoint.starts_with(&g.common))
                {
                    return Err(error("checkout cannot contain the engine socket"));
                }
                c.connection = Some(context.clone());
                let info = json(vec![
                    "docker".into(),
                    "--context".into(),
                    context,
                    "info".into(),
                    "--format=json".into(),
                ])?;
                if cfg!(target_os = "macos") {
                    if !info
                        .get("OperatingSystem")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.starts_with("Docker Desktop"))
                    {
                        return Err(error(
                            "macOS Docker mounts require a local Docker Desktop context",
                        ));
                    }
                    c.options.engine_relay = true;
                }
                let security = info
                    .get("SecurityOptions")
                    .and_then(Value::as_array)
                    .ok_or_else(|| error("Docker omitted namespace capabilities"))?;
                if security
                    .iter()
                    .any(|s| s.as_str().is_some_and(|s| s.contains("userns")))
                {
                    return Err(error(
                        "Docker userns-remap is unsupported for checkout mounts",
                    ));
                }
                let rootless = security
                    .iter()
                    .any(|s| s.as_str().is_some_and(|s| s.contains("rootless")));
                if rootless && fs::metadata(&endpoint).map_err(error)?.uid() != uid {
                    return Err(error("rootless Docker must run as the daemon user"));
                }
                record.spec.workdir = Some(checkout.clone());
                c.workspace = Some(ContainerWorkspace {
                    git,
                    checkout,
                    user: if rootless {
                        "0:0".into()
                    } else {
                        format!("{uid}:{gid}")
                    },
                    keep_id: false,
                    read_only: false,
                    access: Some(WorkspaceAccess {
                        relay: None,
                        grant: String::new(),
                        socket_directory: directory.clone(),
                        directory,
                        vm: None,
                    }),
                });
                return Ok(());
            }
        }
    }
    let socket_directory = if vm.is_some() {
        Path::new("/tmp").join(format!("ad-{}", record.id))
    } else {
        directory.clone()
    };
    record.spec.workdir = Some(checkout.clone());
    c.workspace = Some(ContainerWorkspace {
        git,
        checkout,
        user: format!("{uid}:{gid}"),
        keep_id,
        read_only: false,
        access: Some(WorkspaceAccess {
            relay: None,
            grant: String::new(),
            directory,
            socket_directory,
            vm,
        }),
    });
    Ok(())
}

fn quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}
fn ssh_prefix(vm: &PodmanVm, directory: &Path) -> Vec<String> {
    vec![
        "ssh".into(),
        "-F".into(),
        "/dev/null".into(),
        "-T".into(),
        "-oBatchMode=yes".into(),
        "-oIdentitiesOnly=yes".into(),
        "-oConnectTimeout=5".into(),
        "-oStrictHostKeyChecking=accept-new".into(),
        format!(
            "-oUserKnownHostsFile={}",
            directory.join("known_hosts").display()
        ),
        "-i".into(),
        vm.identity.to_string_lossy().into(),
        "-p".into(),
        vm.port.to_string(),
        format!("{}@127.0.0.1", vm.user),
    ]
}
fn ssh_run(vm: &PodmanVm, directory: &Path, script: &str) -> Result<String, TransportError> {
    let mut args = ssh_prefix(vm, directory);
    args.push(script.into());
    run(args)
}

/// Child ownership and a private control socket let recovery replace only this forward.
pub struct Bridge(Child);
impl Bridge {
    pub fn alive(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
pub fn bridge(
    access: &WorkspaceAccess,
    restricted: &Path,
) -> Result<Option<Bridge>, TransportError> {
    let Some(vm) = &access.vm else {
        return Ok(None);
    };
    let directory = access.directory.parent().unwrap().join(format!(
        "bridge-{}",
        access.directory.file_name().unwrap().to_string_lossy()
    ));
    private_directory(&directory)?;
    let control = directory.join("ctl");
    // OpenSSH adds a random suffix before atomically publishing its control socket.
    if control.as_os_str().len() > 85 {
        return Err(error("daemon home is too long for an SSH control socket"));
    }
    let mut old = ssh_prefix(vm, &directory);
    let destination = old.pop().unwrap();
    old.extend([
        "-S".into(),
        path_arg(&control)?,
        "-O".into(),
        "exit".into(),
        destination.clone(),
    ]);
    let _ = run(old); // Missing master is normal. Never signal a cached PID.
    let dir = quote(&path_arg(&access.socket_directory)?);
    let socket = access.socket_directory.join("endpoint.sock");
    ssh_run(
        vm,
        &directory,
        &format!(
            "umask 077; if test -e {dir}; then test -d {dir} && test ! -L {dir} && test \"$(stat -c '%u:%a' {dir})\" = \"$(id -u):700\" || exit 1; else mkdir -- {dir} || exit 1; fi; rm -f -- {}",
            quote(&path_arg(&socket)?)
        ),
    )?;
    let mut args = ssh_prefix(vm, &directory);
    args.pop();
    args.extend([
        "-M".into(),
        "-S".into(),
        path_arg(&control)?,
        "-oExitOnForwardFailure=yes".into(),
        "-oServerAliveInterval=5".into(),
        "-oServerAliveCountMax=2".into(),
        "-N".into(),
        "-R".into(),
        format!("{}:{}", path_arg(&socket)?, path_arg(restricted)?),
        destination,
    ]);
    let child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(error)?;
    let mut bridge = Bridge(child);
    for _ in 0..50 {
        if !bridge.alive() {
            return Err(error("scoped VM socket forward exited"));
        }
        let mut check = ssh_prefix(vm, &directory);
        let destination = check.pop().unwrap();
        check.extend([
            "-S".into(),
            path_arg(&control)?,
            "-O".into(),
            "check".into(),
            destination,
        ]);
        if run(check).is_ok() && bridge.alive() {
            return Ok(Some(bridge));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(error("VM socket forward did not become ready"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preparation_rollback_removes_owned_paths_and_preserves_existing_or_changed_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let existing = tmp.path().join("existing");
        private_directory(&existing).unwrap();
        let fresh = tmp.path().join("fresh");
        let changed = tmp.path().join("changed");
        let swapped = tmp.path().join("swapped");
        {
            let mut preparation = Preparation::default();
            preparation.directory(&existing).unwrap();
            preparation.directory(&fresh).unwrap();
            preparation.known_hosts(&fresh.join("known_hosts")).unwrap();
            fs::write(fresh.join("known_hosts"), "fixture SSH key").unwrap();
            preparation.directory(&changed).unwrap();
            fs::write(changed.join("keep"), "untracked data").unwrap();
            preparation.directory(&swapped).unwrap();
            fs::rename(&swapped, tmp.path().join("original")).unwrap();
            std::os::unix::fs::symlink(&existing, &swapped).unwrap();
        }
        assert!(!fresh.exists());
        assert!(existing.is_dir());
        assert_eq!(
            fs::read_to_string(changed.join("keep")).unwrap(),
            "untracked data"
        );
        assert!(swapped.is_symlink());
        let committed = tmp.path().join("committed");
        let mut preparation = Preparation::default();
        preparation.directory(&committed).unwrap();
        preparation.commit();
        assert!(committed.is_dir());
    }
    #[test]
    fn private_transport_refuses_symlinks_and_public_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let private = tmp.path().join("private");
        private_directory(&private).unwrap();
        private_directory(&private).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&private, &alias).unwrap();
        assert!(private_directory(&alias).is_err());
        fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            private_directory(&private).unwrap_err().code,
            ErrorCode::StorageUnavailable
        );
        assert_eq!(
            path_arg(Path::new("relative")).unwrap_err().code,
            ErrorCode::Invalid
        );
        for raw in ["relative", "/a,b", "/a:b", "/a\nb"] {
            assert!(path_arg(Path::new(raw)).is_err());
        }
    }
    #[test]
    fn remote_paths_are_literal_shell_arguments() {
        let value = "quote' $(echo unsafe) `echo unsafe`";
        let output = run(vec![
            "sh".into(),
            "-c".into(),
            format!("printf %s {}", quote(value)),
        ])
        .unwrap();
        assert_eq!(output, value);
    }
}
