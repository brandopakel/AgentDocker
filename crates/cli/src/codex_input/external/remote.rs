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
pub(super) struct Record {
    pub version: u32,
    pub provider: ProviderGeneration,
    pub server: ProcessIdentity,
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub port: u16,
    pub token_file: PathBuf,
    pub token_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birth: Option<super::birth::Witness>,
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
        (match (record.version, &record.birth) {
            (1, None) => true,
            (2, Some(birth)) => birth.valid(&record.provider, &record.server),
            _ => false,
        }) && record.provider.valid()
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

fn exact_option<'a>(arguments: &'a [String], key: &str) -> Option<&'a str> {
    let mut found = arguments.iter().enumerate().filter(|(_, arg)| *arg == key);
    let (index, _) = found.next()?;
    if found.next().is_some()
        || arguments
            .iter()
            .any(|arg| arg.starts_with(&format!("{key}=")))
    {
        return None;
    }
    arguments.get(index + 1).map(String::as_str)
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
    let terminal = procinfo::inspect(record.provider.process.pid)
        .context("native terminal arguments unavailable")?;
    let endpoint = format!("ws://127.0.0.1:{}", record.port);
    ensure!(
        process.argv.iter().any(|arg| arg == "app-server")
            && exact_option(&process.argv, "--listen") == Some(endpoint.as_str())
            && exact_option(&process.argv, "--ws-auth") == Some("capability-token")
            && exact_option(&process.argv, "--ws-token-file")
                .and_then(|p| Path::new(p).canonicalize().ok())
                .as_ref()
                == Some(&record.token_file)
            && exact_option(&terminal.argv, "--remote") == Some(endpoint.as_str()),
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

/// Recheck an already initialized service connection's immutable endpoint and
/// kernel generations. Repeating initialize on that connection is invalid.
pub(super) fn reverify(descriptor: &Descriptor, binding: &Binding) -> Result<()> {
    ensure!(descriptor.valid(), "invalid native server descriptor");
    let (record, digest) = read(&descriptor.record)?;
    ensure!(
        digest == descriptor.sha256,
        "native server record changed after binding"
    );
    verify(&descriptor.record, &record, binding)?;
    Ok(())
}

/// Establish an initial anchor from an owned birth receipt, without converting
/// a rejected history request into empty history. Caller must prove its durable
/// ledger has never attempted or disposed of input. Ordinary v1 records cannot
/// use this path; after any attempt, all receipt/recovery reads stay mandatory.
pub(super) async fn fresh_anchor(provider: &mut Provider, binding: &Binding) -> Result<bool> {
    let Some(descriptor) = &binding.remote else {
        return Ok(false);
    };
    ensure!(descriptor.valid(), "invalid native server descriptor");
    let (record, digest) = read(&descriptor.record)?;
    ensure!(
        digest == descriptor.sha256,
        "native server record changed after binding"
    );
    verify(&descriptor.record, &record, binding)?;
    let Some(birth) = &record.birth else {
        return Ok(false);
    };
    if !birth.owns_children(&record.provider, &record.server) {
        return Ok(false);
    }
    let value = provider
        .request(
            "thread/read",
            serde_json::json!({
                "threadId":binding.provider.session,"includeTurns":false
            }),
        )
        .await?;
    if !birth.matches_empty(
        &value["thread"],
        &binding.provider,
        &binding.cwd,
        chrono::Utc::now().timestamp(),
    ) {
        if std::env::var_os("AGENTDOCKER_TRACE_NATIVE_STARTUP").is_some() {
            eprintln!("native-startup: thread metadata no longer matches an empty owned birth");
        }
        return Ok(false);
    }
    let queue = provider
        .request(
            "thread/queue/list",
            serde_json::json!({
                "threadId":binding.provider.session,"limit":1
            }),
        )
        .await?;
    let empty = queue["data"].as_array().is_some_and(Vec::is_empty)
        && queue
            .get("nextCursor")
            .is_some_and(serde_json::Value::is_null);
    if !empty && std::env::var_os("AGENTDOCKER_TRACE_NATIVE_STARTUP").is_some() {
        eprintln!("native-startup: provider queue is not verified empty");
    }
    // Recheck the immutable record's generations after asynchronous reads.
    verify(&descriptor.record, &record, binding)?;
    Ok(empty && birth.owns_children(&record.provider, &record.server))
}

/// Resolve MCP identity only through an already accepted, immutable receiver
/// binding. A server's arguments or tool metadata alone grant no identity.
pub(super) fn verify_mcp_host(
    descriptor: &Descriptor,
    binding: &Binding,
    host: &ProcessIdentity,
    executable: &Path,
    cwd: &Path,
) -> Result<()> {
    ensure!(descriptor.valid(), "invalid native server descriptor");
    let (record, digest) = read(&descriptor.record)?;
    ensure!(
        digest == descriptor.sha256,
        "native server record changed after binding"
    );
    ensure!(
        record.server == *host && record.executable == executable && record.cwd == cwd,
        "Codex MCP host differs from the accepted native server"
    );
    verify(&descriptor.record, &record, binding)?;
    Ok(())
}

pub(super) fn hook_ancestor(descriptor: &Descriptor, binding: &Binding) -> Result<ProcessIdentity> {
    ensure!(descriptor.valid(), "invalid native server descriptor");
    let (record, digest) = read(&descriptor.record)?;
    ensure!(
        digest == descriptor.sha256,
        "native server record changed after binding"
    );
    verify(&descriptor.record, &record, binding)?;
    Ok(record.server)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn native_endpoint_arguments_cannot_be_duplicated_or_hidden_in_another_value() {
        let args = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert_eq!(
            exact_option(
                &args(&["codex", "--remote", "ws://127.0.0.1:1234"]),
                "--remote"
            ),
            Some("ws://127.0.0.1:1234")
        );
        for values in [
            vec!["codex", "--remote"],
            vec!["codex", "--remote=x"],
            vec!["codex", "--remote", "x", "--remote", "y"],
            vec!["codex", "--remote", "x", "--remote=y"],
            vec!["codex", "some --remote x text"],
        ] {
            assert!(exact_option(&args(&values), "--remote").is_none());
        }
    }

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
            birth: None,
        };
        matches_binding(&record, &binding).unwrap();
        let mut owned: Record =
            serde_json::from_value(serde_json::to_value(&record).unwrap()).unwrap();
        owned.version = 2;
        assert!(matches_binding(&owned, &binding).is_err());
        owned.birth = Some(super::super::birth::Witness {
            launcher: ProcessIdentity {
                pid: 3,
                started_at: provider.process.started_at - chrono::Duration::seconds(1),
            },
            created_at: provider.process.started_at.timestamp(),
        });
        matches_binding(&owned, &binding).unwrap();
        owned.version = 1;
        assert!(matches_binding(&owned, &binding).is_err());
        owned.version = 3;
        assert!(matches_binding(&owned, &binding).is_err());
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
        dirs::secure_state_dir(&root).unwrap();
        let path = root.join("server.json");
        let bytes = serde_json::to_vec(&record).unwrap();
        dirs::create_private_file(&path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let descriptor = Descriptor {
            record: path.clone(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        for kind in 0..4 {
            let mut host = record.server.clone();
            let mut executable = record.executable.clone();
            let mut cwd = record.cwd.clone();
            match kind {
                0 => host.pid = record.provider.process.pid,
                1 => host.started_at += chrono::Duration::seconds(1),
                2 => executable = root.join("other-codex"),
                _ => cwd = root.join("other-project"),
            }
            let error =
                verify_mcp_host(&descriptor, &binding, &host, &executable, &cwd).unwrap_err();
            assert!(error.to_string().contains("differs from the accepted"));
        }
        let changed = Descriptor {
            sha256: "0".repeat(64),
            ..descriptor
        };
        assert!(
            verify_mcp_host(
                &changed,
                &binding,
                &record.server,
                &record.executable,
                &record.cwd,
            )
            .unwrap_err()
            .to_string()
            .contains("record changed")
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}
