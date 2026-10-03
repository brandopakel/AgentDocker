//! Extract only the exact portable Windows package. No archive-provided path,
//! link, ACL or permission is handed to a generic filesystem extractor.
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use agentdocker_host::{dirs, files};
use anyhow::{Result, ensure};

const NAMES: &[&str] = &[
    "AgentDocker/agentdocker.exe",
    "AgentDocker/agentd.exe",
    "AgentDocker/agentdocker-ui.exe",
    "AgentDocker/build.json",
    "AgentDocker/README.txt",
    "AgentDocker/licenses/LICENSE-AgentDocker.txt",
    "AgentDocker/licenses/LICENSE-Inter.txt",
];
const MAX_EXPANDED_BYTES: u64 = 210 * 1024 * 1024;

pub(super) fn extract(archive: &Path, destination: &Path) -> Result<PathBuf> {
    let mut file = files::open_regular(archive)?;
    let length = file.metadata()?.len();
    ensure!(
        (22..=80 * 1024 * 1024).contains(&length),
        "Windows ZIP exceeds the download budget"
    );
    // Our seven-file, sub-80MiB packager writes an ordinary single-disk ZIP
    // without a comment or ZIP64. Bound the raw central-directory count before
    // the library allocates entries (and before it folds duplicate names).
    let mut end = [0_u8; 22];
    file.seek(SeekFrom::End(-22))?;
    file.read_exact(&mut end)?;
    let u16_at = |at| u16::from_le_bytes([end[at], end[at + 1]]);
    let u32_at = |at| u32::from_le_bytes([end[at], end[at + 1], end[at + 2], end[at + 3]]);
    ensure!(
        &end[..4] == b"PK\x05\x06"
            && u16_at(4) == 0
            && u16_at(6) == 0
            && usize::from(u16_at(8)) == NAMES.len()
            && usize::from(u16_at(10)) == NAMES.len()
            && u16_at(20) == 0
            && u32_at(12) <= 256 * 1024
            && u64::from(u32_at(12)) + u64::from(u32_at(16)) == length - 22,
        "Windows ZIP has an unsupported central directory"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut archive = zip::ZipArchive::new(file)?;
    ensure!(
        archive.offset() == 0 && !archive.has_overlapping_files()?,
        "Windows ZIP has prefixed or overlapping file data"
    );
    ensure!(
        archive.len() == NAMES.len(),
        "Windows ZIP has an unexpected entry count"
    );
    let mut seen = BTreeSet::new();
    let mut bytes = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        // Exact ASCII names also exclude traversal, alternate data streams,
        // device names, case aliases, trailing dots/spaces and directory entries.
        ensure!(
            NAMES.contains(&entry.name()) && seen.insert(entry.name().to_owned()),
            "Windows ZIP has an unknown or duplicate path"
        );
        ensure!(
            entry.is_file()
                && !entry.is_symlink()
                && entry
                    .unix_mode()
                    .is_none_or(|mode| matches!(mode & 0o170000, 0 | 0o100000)),
            "Windows ZIP contains a link or special file"
        );
        ensure!(!entry.encrypted(), "encrypted Windows ZIP is unsupported");
        ensure!(
            matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            ),
            "unsupported Windows ZIP compression"
        );
        bytes = bytes
            .checked_add(entry.size())
            .ok_or_else(|| anyhow::anyhow!("Windows ZIP size overflow"))?;
        ensure!(
            bytes <= MAX_EXPANDED_BYTES,
            "Windows ZIP exceeds the expanded payload budget"
        );
    }
    // A fresh directory keeps concurrent update attempts separate and refuses
    // a pre-existing path rather than deleting or overwriting its contents.
    match destination.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        _ => anyhow::bail!(
            "Windows ZIP extraction destination already exists or cannot be inspected"
        ),
    }
    dirs::secure_state_dir(destination)?;
    let payload = destination.join("AgentDocker");
    dirs::secure_state_dir(&payload)?;
    dirs::secure_state_dir(&payload.join("licenses"))?;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let path = destination.join(entry.name());
        let expected = entry.size();
        let mut target = dirs::create_private_file(&path)?;
        let copied = std::io::copy(&mut entry.take(expected + 1), &mut target)?;
        ensure!(
            copied == expected,
            "Windows ZIP entry changed its declared size"
        );
        target.flush()?;
        target.sync_all()?;
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    fn fixture(names: &[&str], link: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("preview.zip");
        let mut zip = ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (index, name) in names.iter().enumerate() {
            if link && index == 0 {
                zip.add_symlink(*name, "../escape", options).unwrap();
            } else {
                zip.start_file(*name, options).unwrap();
                zip.write_all(b"fixture file").unwrap();
            }
        }
        zip.finish().unwrap();
        let destination = temp.path().join("fresh payload");
        (temp, archive, destination)
    }

    #[test]
    fn exact_windows_payload_is_extracted_without_overwriting_existing_files() {
        let (_temp, archive, destination) = fixture(NAMES, false);
        let payload = extract(&archive, &destination).unwrap();
        assert_eq!(
            std::fs::read(payload.join("agentdocker.exe")).unwrap(),
            b"fixture file"
        );
        std::fs::write(payload.join("README.txt"), b"preserve changed file").unwrap();
        assert!(extract(&archive, &destination).is_err());
        assert_eq!(
            std::fs::read(payload.join("README.txt")).unwrap(),
            b"preserve changed file"
        );
    }

    #[test]
    fn windows_aliases_traversal_and_unknown_entries_are_refused_before_creation() {
        for invalid in [
            "../escape",
            "C:/escape",
            "AgentDocker/../escape",
            "AgentDocker/AGENTD.EXE",
            "AgentDocker/agentd.exe:stream",
            "AgentDocker/CON",
            "AgentDocker/agentd.exe.",
            "AgentDocker/agentd.exe ",
            "AgentDocker\\agentd.exe",
            "AgentDocker/extra.txt",
        ] {
            let mut names = NAMES.to_vec();
            names[0] = invalid;
            let (_temp, archive, destination) = fixture(&names, false);
            assert!(
                extract(&archive, &destination).is_err(),
                "accepted {invalid}"
            );
            assert!(!destination.exists());
        }
    }

    #[test]
    fn links_and_incomplete_windows_packages_are_refused_before_creation() {
        for (names, link) in [(NAMES, true), (&NAMES[..NAMES.len() - 1], false)] {
            let (_temp, archive, destination) = fixture(names, link);
            assert!(extract(&archive, &destination).is_err());
            assert!(!destination.exists());
        }
    }

    #[test]
    fn expanded_windows_zip_budget_is_checked_before_writing() {
        let (_temp, archive, destination) = fixture(NAMES, false);
        let mut bytes = std::fs::read(&archive).unwrap();
        let central = bytes
            .windows(4)
            .position(|part| part == b"PK\x01\x02")
            .unwrap();
        bytes[central + 24..central + 28]
            .copy_from_slice(&((MAX_EXPANDED_BYTES + 1) as u32).to_le_bytes());
        std::fs::write(&archive, bytes).unwrap();
        assert!(extract(&archive, &destination).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn raw_zip_entry_count_is_bounded_before_library_name_folding() {
        let (_temp, archive, destination) = fixture(NAMES, false);
        let mut bytes = std::fs::read(&archive).unwrap();
        let end = bytes.len() - 22;
        bytes[end + 8..end + 12].copy_from_slice(&[255, 255, 255, 255]);
        std::fs::write(&archive, bytes).unwrap();
        assert!(extract(&archive, &destination).is_err());
        assert!(!destination.exists());
    }
}
