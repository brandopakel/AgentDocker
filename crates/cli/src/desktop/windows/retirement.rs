//! Loaded bootstrap images leave their public names synchronously. Verified
//! private copies are collected after Windows releases their image sections.
use super::*;

const MAX_GENERATIONS: usize = 8;
const MAX_BYTES: u64 = 256 * 1024 * 1024;

fn inventory(layout: &Layout) -> Result<Vec<PathBuf>> {
    let root = layout.root.join("retired-launchers");
    if !present(&root)? {
        return Ok(Vec::new());
    }
    dirs::check_private_dir(&root)?;
    let entries = std::fs::read_dir(root)?
        .take(MAX_GENERATIONS + 1)
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        entries.len() <= MAX_GENERATIONS,
        "retired launcher inventory exceeds its bound; preserved"
    );
    Ok(entries.into_iter().map(|entry| entry.path()).collect())
}

fn checked(path: &Path) -> Result<Vec<PathBuf>> {
    let id = path
        .file_name()
        .and_then(|s| s.to_str())
        .context("invalid retired launcher directory")?;
    ensure!(
        uuid::Uuid::parse_str(id)?.to_string() == id,
        "unrecognized retirement directory; preserved"
    );
    dirs::check_private_dir(path)?;
    let receipt: serde_json::Value = record(&path.join("launcher.json"))?
        .context("retired launchers have no ownership receipt; preserved")?;
    let hashes = receipt["binary_sha256"]
        .as_object()
        .context("invalid retirement receipt")?;
    ensure!(
        receipt["format"] == 1
            && hashes.len() == BINARIES.len()
            && BINARIES.iter().all(
                |name| hashes
                    .get(*name)
                    .and_then(|v| v.as_str())
                    .is_some_and(|h| h.len() == 64
                        && h.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
            ),
        "unrecognized retirement receipt; preserved"
    );
    let entries = std::fs::read_dir(path)?
        .take(BINARIES.len() + 2)
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        entries.len() <= BINARIES.len() + 1,
        "unrecognized retired content; preserved"
    );
    let mut files = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        if name == "launcher.json" {
            continue;
        }
        let name = name
            .to_str()
            .filter(|s| BINARIES.contains(s))
            .context("unrecognized retired content; preserved")?;
        let path = entry.path();
        let file = dirs::open_private_snapshot(&path)?;
        ensure!(
            file.metadata()?.len() <= MAX_BYTES,
            "retired launcher exceeds its size bound; preserved"
        );
        drop(file);
        ensure!(
            Some(file_hash(&path)?.as_str()) == hashes[name].as_str(),
            "modified retired launcher; preserved"
        );
        files.push(path);
    }
    Ok(files)
}

/// Call only while holding the installation lock. Any foreign/changed content
/// prevents that directory's cleanup; a loaded image is retained for a later call.
pub(super) fn cleanup(layout: &Layout) -> Result<()> {
    for path in inventory(layout)? {
        let files = checked(&path)?;
        let mut retained = false;
        for file in files {
            match std::fs::remove_file(&file) {
                Ok(()) => (),
                Err(error) if matches!(error.raw_os_error(), Some(5 | 32)) => retained = true,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("remove retired launcher {}", file.display()));
                }
            }
        }
        if !retained {
            // All names and bytes were checked before removing any of them.
            std::fs::remove_file(path.join("launcher.json"))?;
            std::fs::remove_dir(path)?;
        }
    }
    Ok(())
}

/// Reserve a bounded, private destination before deactivation. The receipt is
/// durable before the first move, so an interrupted uninstall remains verifiable.
pub(super) fn prepare(layout: &Layout, launchers: &[PathBuf]) -> Result<Option<PathBuf>> {
    cleanup(layout)?;
    if launchers.is_empty() {
        return Ok(None);
    }
    let previous = inventory(layout)?;
    ensure!(
        previous.len() < MAX_GENERATIONS,
        "retired launchers are still in use; close them and run desktop prune before uninstalling"
    );
    let mut bytes = 0_u64;
    for path in previous {
        for file in checked(&path)? {
            bytes = bytes
                .checked_add(std::fs::metadata(file)?.len())
                .context("retirement size overflow")?;
        }
    }
    for file in launchers {
        bytes = bytes
            .checked_add(std::fs::metadata(file)?.len())
            .context("retirement size overflow")?;
    }
    ensure!(
        bytes <= MAX_BYTES,
        "retired launcher storage would exceed 256 MiB; close old launchers and run desktop prune first"
    );
    let receipt: serde_json::Value = record(&layout.root.join("launcher.json"))?
        .context("launcher ownership receipt is missing")?;
    let root = layout.root.join("retired-launchers");
    dirs::secure_state_dir(&root)?;
    let stage = root.join(uuid::Uuid::new_v4().to_string());
    dirs::secure_state_dir(&stage)?;
    publish(&stage, "launcher.json", &receipt)?;
    Ok(Some(stage))
}
