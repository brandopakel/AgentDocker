//! A private immutable descriptor for a native TUI's authenticated app-server.
use super::{Provider, ledger::Binding};
use agentdocker_core::{ProcessIdentity, ProviderGeneration};
use agentdocker_host::{dirs, procinfo};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

const MAX_RECORD: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Descriptor {
    pub record: PathBuf,
    pub sha256: String,
}

impl Descriptor {
    pub(super) fn valid(&self) -> bool {
        self.record.is_absolute()
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

fn confidential(metadata: &std::fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "native server capability storage must be accessible only to its owner"
        );
    }
    // Windows private-file/directory helpers verify the protected owner ACL.
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    provider: ProviderGeneration,
    server: ProcessIdentity,
    executable: PathBuf,
    cwd: PathBuf,
    port: u16,
    token_file: PathBuf,
    token_sha256: String,
}

fn read(path: &Path) -> Result<(Record, String)> {
    ensure!(path.is_absolute(), "native server record must be absolute");
    let parent = path
        .parent()
        .context("native server record has no parent")?;
    dirs::check_private_dir(parent)?;
    confidential(&std::fs::symlink_metadata(parent)?)?;
    let file = dirs::open_private_snapshot(path)?;
    confidential(&file.metadata()?)?;
    let mut data = Vec::new();
    file.take(MAX_RECORD + 1).read_to_end(&mut data)?;
    ensure!(
        data.len() as u64 <= MAX_RECORD,
        "native server record exceeds size limit"
    );
    let digest = format!("{:x}", Sha256::digest(&data));
    Ok((
        serde_json::from_slice(&data).context("invalid native server record")?,
        digest,
    ))
}

fn matches_binding(record: &Record, binding: &Binding) -> Result<()> {
    ensure!(
        record.version == 1
            && record.provider.valid()
            && record.server.pid > 0
            && record.provider == binding.provider
            && record.cwd == binding.cwd
            && record.executable == binding.executable
            && record.port != 0
            && record.server.pid != record.provider.process.pid
            && record.token_file.is_absolute()
            && record.token_sha256.len() == 64
            && record.token_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "native server record differs from the exact provider binding"
    );
    Ok(())
}

fn verify(path: &Path, record: &Record, binding: &Binding) -> Result<String> {
    matches_binding(record, binding)?;
    ensure!(
        procinfo::start_time(record.server.pid) == Some(record.server.started_at)
            && procinfo::start_time(record.provider.process.pid)
                == Some(record.provider.process.started_at),
        "native server or terminal generation has exited"
    );
    ensure!(
        procinfo::executable_path_of(record.server.pid)?.canonicalize()? == record.executable
            && procinfo::executable_path_of(record.provider.process.pid)?.canonicalize()?
                == record.executable
            && procinfo::cwd(record.server.pid)
                .and_then(|p| p.canonicalize().ok())
                .as_ref()
                == Some(&record.cwd),
        "native server or terminal image/checkout changed"
    );
    let root = path
        .parent()
        .context("native server record has no parent")?
        .canonicalize()?;
    ensure!(
        record
            .token_file
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .as_ref()
            == Some(&root),
        "native server capability must share the private record directory"
    );
    let process =
        procinfo::inspect(record.server.pid).context("native server arguments unavailable")?;
    let pair = |key: &str, value: &str| {
        process
            .argv
            .windows(2)
            .any(|p| p[0] == key && p[1] == value)
    };
    ensure!(
        process.argv.iter().any(|arg| arg == "app-server")
            && pair("--listen", &format!("ws://127.0.0.1:{}", record.port))
            && pair("--ws-auth", "capability-token")
            && process.argv.windows(2).any(|p| p[0] == "--ws-token-file"
                && Path::new(&p[1]).canonicalize().ok().as_ref() == Some(&record.token_file)),
        "native server launch does not match the private endpoint/capability"
    );
    let token_file = dirs::read_private_file(&record.token_file)?;
    confidential(&token_file.metadata()?)?;
    let mut token = String::new();
    token_file.take(257).read_to_string(&mut token)?;
    ensure!(
        (32..=256).contains(&token.len())
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && format!("{:x}", Sha256::digest(token.as_bytes())) == record.token_sha256,
        "native server capability changed or is invalid"
    );
    ensure!(
        procinfo::start_time(record.server.pid) == Some(record.server.started_at)
            && procinfo::start_time(record.provider.process.pid)
                == Some(record.provider.process.started_at),
        "native server generation changed during verification"
    );
    Ok(token)
}

pub(super) fn describe(path: &Path, binding: &Binding) -> Result<Descriptor> {
    let (record, sha256) = read(path)?;
    verify(path, &record, binding)?;
    Ok(Descriptor {
        record: path.canonicalize()?,
        sha256,
    })
}

pub(super) async fn connect(descriptor: &Descriptor, binding: &Binding) -> Result<Provider> {
    ensure!(descriptor.valid(), "invalid native server descriptor");
    let (record, digest) = read(&descriptor.record)?;
    ensure!(
        digest == descriptor.sha256,
        "native server record changed after binding"
    );
    let token = verify(&descriptor.record, &record, binding)?;
    let provider = Provider::connect_native(record.port, &token).await?;
    // A slow connection must not accept a generation which exited meanwhile.
    verify(&descriptor.record, &record, binding)?;
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn descriptor_reads_are_bounded_private_and_reject_links() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        dirs::secure_state_dir(&root).unwrap();
        let path = root.join("server.json");
        let mut file = dirs::create_private_file(&path).unwrap();
        file.write_all(&vec![b' '; MAX_RECORD as usize + 1])
            .unwrap();
        drop(file);
        assert!(
            read(&path)
                .err()
                .unwrap()
                .to_string()
                .contains("size limit")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let link = root.join("alias.json");
            symlink(&path, &link).unwrap();
            assert!(read(&link).is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                read(&path)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("only to its owner")
            );
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(
                read(&path)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("only to its owner")
            );
        }
        assert!(
            !Descriptor {
                record: "relative".into(),
                sha256: "a".repeat(64)
            }
            .valid()
        );
        assert!(
            !Descriptor {
                record: path,
                sha256: "not-a-digest".into()
            }
            .valid()
        );
    }

    #[test]
    fn records_are_bound_to_both_processes_profile_thread_checkout_and_image() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let provider = ProviderGeneration {
            process: ProcessIdentity {
                pid: 1,
                started_at: chrono::Utc::now(),
            },
            session: "thread".into(),
            profile: root.to_string_lossy().into(),
        };
        let binding = Binding {
            agent: "agent".into(),
            provider: provider.clone(),
            socket: root.join("sock"),
            cwd: root.clone(),
            executable: root.join("codex"),
            remote: None,
        };
        let record = Record {
            version: 1,
            provider: provider.clone(),
            server: ProcessIdentity {
                pid: 2,
                started_at: provider.process.started_at,
            },
            executable: binding.executable.clone(),
            cwd: root.clone(),
            port: 1234,
            token_file: root.join("token"),
            token_sha256: "a".repeat(64),
        };
        matches_binding(&record, &binding).unwrap();
        for kind in 0..7 {
            let mut r: Record =
                serde_json::from_value(serde_json::to_value(&record).unwrap()).unwrap();
            match kind {
                0 => r.provider.session = "different".into(),
                1 => r.provider.profile = "different".into(),
                2 => r.provider.process.started_at += chrono::Duration::seconds(1),
                3 => r.cwd = root.join("different"),
                4 => r.executable = root.join("different"),
                5 => r.server.pid = r.provider.process.pid,
                _ => r.port = 0,
            }
            assert!(matches_binding(&r, &binding).is_err());
        }
        assert!(verify(&root.join("server.json"), &record, &binding).is_err());
    }
}
