//! Shared Docker/Podman build adapter with captured, bounded input provenance.
use crate::command;
use agentdocker_core::{ContainerEngine, ImageBuild, ImageBuildSpec};
use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::Duration,
};

const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("invalid build input: {0}")]
    Input(io::Error),
    #[error("host I/O failed: {0}")]
    Host(#[from] io::Error),
    #[error("container engine unavailable: {0}")]
    Unavailable(String),
    #[error("container build failed: {0}")]
    Build(String),
    #[error("container build evidence is invalid: {0}")]
    Evidence(String),
}

/// Run the explicitly selected engine; never try another engine on failure.
pub fn build(spec: ImageBuildSpec, id: String) -> Result<ImageBuild, EngineError> {
    if spec.engine == ContainerEngine::Docker
        && std::env::var("DOCKER_BUILDKIT").as_deref() == Ok("0")
    {
        return Err(EngineError::Build(
            "Docker image builds require Buildx; DOCKER_BUILDKIT=0 disables the supported builder"
                .into(),
        ));
    }
    build_with(spec, id, &mut |root, argv, timeout| {
        command::run(root, argv, timeout)
    })
}

type RunResult = io::Result<command::Output>;
fn build_with(
    spec: ImageBuildSpec,
    id: String,
    run: &mut impl FnMut(&Path, &[String], Duration) -> RunResult,
) -> Result<ImageBuild, EngineError> {
    if !(1..=3600).contains(&spec.timeout_secs)
        || spec
            .connection
            .as_ref()
            .is_some_and(|c| c.is_empty() || c.starts_with('-'))
    {
        return Err(EngineError::Input(io::Error::other(
            "timeout must be 1–3600 seconds and connection must be a nonempty name",
        )));
    }
    if spec.recipe.as_os_str().is_empty()
        || spec
            .recipe
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(EngineError::Input(io::Error::other(
            "recipe must be relative to the build context without parent traversal",
        )));
    }
    let context = spec.context.canonicalize().map_err(EngineError::Input)?;
    if !context.is_dir() {
        return Err(EngineError::Input(io::Error::other(
            "build context must be a directory",
        )));
    }
    let mut prefix = vec![spec.engine.to_string()];
    if let Some(connection) = &spec.connection {
        prefix.push(
            match spec.engine {
                ContainerEngine::Docker => "--context",
                ContainerEngine::Podman => "--connection",
            }
            .into(),
        );
        prefix.push(connection.clone());
    }
    let execute = |suffix: Vec<String>,
                   timeout,
                   run: &mut dyn FnMut(&Path, &[String], Duration) -> RunResult| {
        let mut argv = prefix.clone();
        argv.extend(suffix);
        run(&context, &argv, timeout)
    };
    let version = execute(
        vec![
            "version".into(),
            "--format".into(),
            match spec.engine {
                ContainerEngine::Docker => "{{json .}}",
                ContainerEngine::Podman => "json",
            }
            .into(),
        ],
        Duration::from_secs(15),
        run,
    )
    .map_err(|e| EngineError::Unavailable(e.to_string()))?;
    if !version.success {
        return Err(EngineError::Unavailable(version.text));
    }
    let version: Value = serde_json::from_str(&version.stdout)
        .map_err(|e| EngineError::Evidence(format!("version response: {e}")))?;
    let client_version = version
        .pointer("/Client/Version")
        .and_then(Value::as_str)
        .ok_or_else(|| EngineError::Evidence("engine did not report its client version".into()))?
        .to_owned();
    let server_version = version
        .pointer("/Server/Version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if spec.engine == ContainerEngine::Docker {
        let builder = execute(
            vec!["buildx".into(), "inspect".into(), "--bootstrap".into()],
            Duration::from_secs(30),
            run,
        )
        .map_err(|e| EngineError::Build(format!("Buildx builder unavailable: {e}")))?;
        if !builder.success {
            return Err(EngineError::Build(format!(
                "Buildx builder unavailable: {}",
                builder.text
            )));
        }
    }
    let captured_at = Utc::now();
    let snapshot = capture(&context)?;
    let recipe = snapshot.dir.path().join(&spec.recipe);
    let metadata = fs::symlink_metadata(&recipe).map_err(EngineError::Input)?;
    if !metadata.is_file() {
        return Err(EngineError::Input(io::Error::other(
            "recipe must be a regular captured file",
        )));
    }
    let recipe_version = digest(&fs::read(&recipe)?);
    let output_dir = tempfile::tempdir()?;
    let iidfile = output_dir.path().join("image-id");
    let mut args = vec![
        "build".into(),
        "--iidfile".into(),
        iidfile.to_string_lossy().into_owned(),
        "--file".into(),
        recipe.to_string_lossy().into_owned(),
    ];
    if spec.engine == ContainerEngine::Docker {
        args.insert(0, "buildx".into());
        args.push("--load".into());
    }
    args.push(snapshot.dir.path().to_string_lossy().into_owned());
    let output = execute(args, Duration::from_secs(spec.timeout_secs), run)
        .map_err(|e| EngineError::Build(e.to_string()))?;
    if !output.success {
        return Err(EngineError::Build(output.text));
    }
    let image_id = image_id(
        fs::read_to_string(&iidfile)
            .map_err(|e| EngineError::Evidence(format!("image ID could not be read: {e}")))?
            .trim(),
    )?;
    let inspected = execute(
        vec!["image".into(), "inspect".into(), image_id.clone()],
        Duration::from_secs(15),
        run,
    )
    .map_err(|e| EngineError::Unavailable(e.to_string()))?;
    if !inspected.success {
        return Err(EngineError::Evidence(inspected.text));
    }
    let inspected: Value = serde_json::from_str(&inspected.stdout)
        .map_err(|e| EngineError::Evidence(format!("image inspect response: {e}")))?;
    let images = inspected
        .as_array()
        .filter(|images| images.len() == 1)
        .ok_or_else(|| EngineError::Evidence("expected exactly one inspected image".into()))?;
    let inspected = &images[0];
    let field = |key: &str| {
        inspected
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| EngineError::Evidence(format!("image inspect omitted {key}")))
    };
    if image_id != self::image_id(&field("Id")?)? {
        return Err(EngineError::Evidence(
            "built and inspected image IDs differ".into(),
        ));
    }
    Ok(ImageBuild {
        id,
        spec: ImageBuildSpec { context, ..spec },
        captured_at,
        finished_at: Utc::now(),
        context_version: snapshot.version,
        recipe_version,
        image_id,
        client_version,
        server_version,
        os: field("Os")?,
        architecture: field("Architecture")?,
        variant: inspected
            .get("Variant")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    })
}

fn image_id(raw: &str) -> Result<String, EngineError> {
    let hash = raw.strip_prefix("sha256:").unwrap_or(raw);
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(EngineError::Evidence(
            "engine returned a malformed image ID".into(),
        ));
    }
    Ok(format!("sha256:{hash}"))
}
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

struct Captured {
    dir: tempfile::TempDir,
    version: String,
}
/// Capture actual bytes consumed by the engine, including files ignored by Git.
/// Engine-specific ignore files are preserved and interpreted by that engine.
fn capture(root: &Path) -> Result<Captured, EngineError> {
    let dir = tempfile::tempdir()?;
    let mut entries = BTreeMap::<PathBuf, Vec<u8>>::new();
    let mut budget = MAX_BYTES;
    for entry in ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .hidden(false)
        .follow_links(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
    {
        let entry = entry.map_err(io::Error::other)?;
        let relative = entry.path().strip_prefix(root).map_err(io::Error::other)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        if entries.len() >= MAX_FILES {
            return Err(EngineError::Input(io::Error::other(
                "build context exceeds 20000 entries; choose a smaller context",
            )));
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        let out = dir.path().join(relative);
        let record = if metadata.is_dir() {
            fs::create_dir(&out)?;
            fs::set_permissions(&out, fs::Permissions::from_mode(0o755))?;
            b"directory:755".to_vec()
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(entry.path())?;
            if target.is_absolute()
                || !crate::project::try_canonical(entry.path())?.starts_with(root)
            {
                return Err(EngineError::Input(io::Error::other(
                    "build context symlink escapes its root",
                )));
            }
            std::os::unix::fs::symlink(&target, &out)?;
            let mut record = b"symlink:".to_vec();
            record.extend(target.as_os_str().as_encoded_bytes());
            record
        } else {
            let mut input = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(entry.path())?;
            let before = input.metadata()?;
            if !before.is_file() || before.len() > budget {
                return Err(EngineError::Input(io::Error::other(
                    "build context needs regular files within 256 MiB",
                )));
            }
            let mut data = Vec::new();
            Read::by_ref(&mut input)
                .take(budget + 1)
                .read_to_end(&mut data)?;
            budget = budget
                .checked_sub(data.len() as u64)
                .ok_or_else(|| io::Error::other("build context exceeds 256 MiB"))?;
            use std::os::unix::fs::MetadataExt;
            let after = input.metadata()?;
            if before.len() != after.len()
                || before.mtime() != after.mtime()
                || before.mtime_nsec() != after.mtime_nsec()
                || before.ctime() != after.ctime()
                || before.ctime_nsec() != after.ctime_nsec()
            {
                return Err(EngineError::Input(io::Error::other(
                    "build input changed during capture; retry",
                )));
            }
            let mode = before.permissions().mode() & 0o777;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&out)?;
            file.write_all(&data)?;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
            format!("file:{mode:o}:{}", digest(&data)).into_bytes()
        };
        entries.insert(relative.to_path_buf(), record);
    }
    // Build tools may preserve mtimes in image layers. Normalize them rather
    // than introducing capture-time metadata absent from the content identity.
    for relative in entries.keys().rev().chain(std::iter::once(&PathBuf::new())) {
        let path = dir.path().join(relative);
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(io::Error::other)?;
        let times = [libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }; 2];
        // SAFETY: the path is NUL-terminated and times holds both required entries.
        if unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                path.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    let mut hash = Sha256::new();
    for (path, record) in entries {
        let path = path.as_os_str().as_encoded_bytes();
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path);
        hash.update((record.len() as u64).to_le_bytes());
        hash.update(record);
    }
    Ok(Captured {
        dir,
        version: format!("sha256:{:x}", hash.finalize()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(engine: ContainerEngine) -> (tempfile::TempDir, ImageBuildSpec) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Containerfile"),
            "FROM scratch\nCOPY input /input\n",
        )
        .unwrap();
        fs::write(dir.path().join("input"), "captured").unwrap();
        let spec = ImageBuildSpec {
            engine,
            connection: Some("test-engine".into()),
            context: dir.path().to_path_buf(),
            recipe: "Containerfile".into(),
            timeout_secs: 60,
        };
        (dir, spec)
    }

    #[test]
    fn captured_context_includes_ignored_inputs_and_rejects_escaping_symlinks() {
        let (dir, _) = fixture(ContainerEngine::Podman);
        fs::write(dir.path().join(".gitignore"), "input\n").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/private"), "not a build input").unwrap();
        let before = capture(dir.path()).unwrap();
        assert!(!before.dir.path().join(".git").exists());
        fs::write(dir.path().join("input"), "changed").unwrap();
        assert_eq!(
            fs::read_to_string(before.dir.path().join("input")).unwrap(),
            "captured"
        );
        assert_ne!(before.version, capture(dir.path()).unwrap().version);
        std::os::unix::fs::symlink("../../outside", dir.path().join("escape")).unwrap();
        assert!(capture(dir.path()).is_err());
    }

    #[test]
    fn capture_refuses_special_files_without_blocking() {
        let (dir, _) = fixture(ContainerEngine::Docker);
        let pipe =
            std::ffi::CString::new(dir.path().join("pipe").as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated fixture path.
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        assert!(capture(dir.path()).is_err());
    }

    #[test]
    fn both_adapters_build_captured_inputs_and_verify_immutable_identity() {
        for engine in [ContainerEngine::Docker, ContainerEngine::Podman] {
            for mismatched in [false, true] {
                let (dir, spec) = fixture(engine);
                let expected = capture(dir.path()).unwrap().version;
                let mut calls = 0;
                let result = build_with(spec, "build-id".into(), &mut |_, argv, _| {
                    calls += 1;
                    assert_eq!(argv[0], engine.to_string());
                    assert_eq!(
                        argv[1],
                        if engine == ContainerEngine::Docker {
                            "--context"
                        } else {
                            "--connection"
                        }
                    );
                    assert_eq!(argv[2], "test-engine");
                    let text = if argv[3] == "version" {
                        json!({"Client":{"Version":"1"},"Server":{"Version":"2"}}).to_string()
                    } else if argv[3] == "buildx" && argv[4] == "inspect" {
                        "Driver: docker".into()
                    } else if argv.iter().any(|arg| arg == "--iidfile") {
                        assert_eq!(
                            argv.iter().any(|s| s == "--load"),
                            engine == ContainerEngine::Docker
                        );
                        let captured = Path::new(argv.last().unwrap());
                        fs::write(dir.path().join("input"), "concurrent change")?;
                        assert_eq!(fs::read_to_string(captured.join("input"))?, "captured");
                        let iid = argv.iter().position(|s| s == "--iidfile").unwrap();
                        fs::write(&argv[iid + 1], "a".repeat(64))?;
                        String::new()
                    } else {
                        assert_eq!(argv[3..5], ["image", "inspect"]);
                        assert_eq!(argv[5], format!("sha256:{}", "a".repeat(64)));
                        json!([{"Id":format!("sha256:{}", if mismatched { "b" } else { "a" }.repeat(64)),"Os":"linux","Architecture":"arm64"}]).to_string()
                    };
                    Ok(command::Output {
                        success: true,
                        stdout: text.clone(),
                        text: format!("{text}\nengine warning on stderr"),
                    })
                });
                assert_eq!(
                    calls,
                    if engine == ContainerEngine::Docker {
                        4
                    } else {
                        3
                    }
                );
                if mismatched {
                    assert!(matches!(result, Err(EngineError::Evidence(_))));
                } else {
                    let built = result.unwrap();
                    assert_eq!(built.context_version, expected);
                    assert_eq!(built.image_id, format!("sha256:{}", "a".repeat(64)));
                    assert_eq!(built.server_version.as_deref(), Some("2"));
                    assert_eq!(built.architecture, "arm64");
                }
            }
        }
    }

    #[test]
    fn unavailable_engine_never_falls_back_and_invalid_recipe_never_runs() {
        let (_dir, mut spec) = fixture(ContainerEngine::Podman);
        let mut calls = 0;
        let mut unavailable = |_: &Path, argv: &[String], _: Duration| {
            calls += 1;
            assert_eq!(argv[0], "podman");
            Err(io::Error::new(io::ErrorKind::NotFound, "engine missing"))
        };
        assert!(matches!(
            build_with(spec.clone(), "id".into(), &mut unavailable),
            Err(EngineError::Unavailable(_))
        ));
        spec.recipe = "../outside".into();
        assert!(matches!(
            build_with(spec, "id".into(), &mut unavailable),
            Err(EngineError::Input(_))
        ));
        assert_eq!(calls, 1);
    }
    #[test]
    fn unsupported_docker_builder_is_rejected_before_build_execution() {
        let (_dir, spec) = fixture(ContainerEngine::Docker);
        let mut calls = 0;
        let result = build_with(spec, "id".into(), &mut |_, argv, _| {
            calls += 1;
            assert!(!argv.iter().any(|arg| arg == "--iidfile"));
            let (success, stdout) = if argv[3] == "version" {
                (true, json!({"Client":{"Version":"1"}}).to_string())
            } else {
                assert_eq!(argv[3..], ["buildx", "inspect", "--bootstrap"]);
                (false, "Buildx is unavailable".into())
            };
            Ok(command::Output {
                success,
                text: stdout.clone(),
                stdout,
            })
        });
        assert!(matches!(result, Err(EngineError::Build(_))));
        assert_eq!(calls, 2);
    }
}
