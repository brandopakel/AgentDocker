//! `agentdocker desktop update`: read the published download feed, fetch the
//! archive for this machine, prove it is the one the feed describes, and hand
//! the extracted payload to the installer that already knows how to preview,
//! pin and activate a release.
//!
//! The feed is the one `packaging/desktop/feed.py` writes: one version across
//! all targets, each archive with its byte count and SHA-256. Fetching uses
//! the system `curl` with HTTPS-only, redirect-limited, size-capped options,
//! so no new network stack ships in the CLI. A `file://` feed exists for the
//! offline smoke and is accepted only alongside `--local-preview`, the same
//! flag that already admits ad-hoc-signed builds.
//!
//! Nothing here restarts a daemon or stops an agent. The report says whether
//! agents are live so the person can decide when to restart.
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agentdocker_core::{Request, Response};
use agentdocker_host::{command, dirs};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Activation, Layout, inspect, perform};

/// Where releases advertise themselves. GitHub redirects this to the asset of
/// the newest release, so the URL never changes.
pub const DEFAULT_FEED: &str =
    "https://github.com/brandopakel/AgentDocker/releases/latest/download/updates.json";
/// Environment override for the feed, so a fixture can point the app at a
/// local feed without a flag reaching through the desktop screen.
pub const FEED_ENV: &str = "AGENTDOCKER_UPDATE_FEED";
const FEED_MAX_BYTES: u64 = 128 * 1024;
/// The largest archive the feed may ask us to download.
const ARCHIVE_MAX_BYTES: u64 = 80 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Options {
    pub feed: String,
    pub check: bool,
    pub apply: bool,
    pub local_preview: bool,
    pub socket: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct Feed {
    format: u32,
    product: String,
    channel: String,
    #[serde(default)]
    policy: Value,
    releases: Vec<FeedRelease>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct FeedRelease {
    target: String,
    version: String,
    source_commit: String,
    state_schema: u32,
    #[serde(default)]
    signing: String,
    #[serde(default)]
    notarized: bool,
    archive: Archive,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Archive {
    name: String,
    sha256: String,
    bytes: u64,
    url: String,
}

/// The Rust target triple the installer accepts on this machine.
fn host_target() -> &'static str {
    match (cfg!(target_os = "macos"), cfg!(target_arch = "aarch64")) {
        (true, true) => "aarch64-apple-darwin",
        (true, false) => "x86_64-apple-darwin",
        (false, true) => "aarch64-unknown-linux-gnu",
        (false, false) => "x86_64-unknown-linux-gnu",
    }
}

/// `MAJOR.MINOR.PATCH[-pre][+build]`, compared the way semantic versioning
/// says: numbers first, a pre-release below its release, build ignored.
fn compare_versions(a: &str, b: &str) -> Ordering {
    fn split(version: &str) -> (Vec<u64>, Option<String>) {
        let core = version.split('+').next().unwrap_or(version);
        let (numbers, pre) = match core.split_once('-') {
            Some((numbers, pre)) => (numbers, Some(pre.to_owned())),
            None => (core, None),
        };
        (
            numbers
                .split('.')
                .map(|part| part.parse::<u64>().unwrap_or(0))
                .collect(),
            pre,
        )
    }
    let (na, pa) = split(a);
    let (nb, pb) = split(b);
    match na.cmp(&nb) {
        Ordering::Equal => match (pa, pb) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(x), Some(y)) => x.cmp(&y),
        },
        other => other,
    }
}

fn curl_binary() -> String {
    if cfg!(target_os = "macos") {
        "/usr/bin/curl".into()
    } else {
        "curl".into()
    }
}

/// The exact `curl` invocation for one download: HTTPS only, including after
/// redirects, a bounded size, and a bounded time.
fn curl_argv(url: &str, destination: &Path, max_bytes: u64) -> Vec<String> {
    vec![
        curl_binary(),
        "--fail".into(),
        "--silent".into(),
        "--show-error".into(),
        "--location".into(),
        "--max-redirs".into(),
        "5".into(),
        "--proto".into(),
        "=https".into(),
        "--proto-redir".into(),
        "=https".into(),
        "--tlsv1.2".into(),
        "--max-time".into(),
        FETCH_TIMEOUT.as_secs().to_string(),
        "--max-filesize".into(),
        max_bytes.to_string(),
        "--output".into(),
        destination.to_string_lossy().into_owned(),
        "--".into(),
        url.into(),
    ]
}

/// Read a URL into `destination`. `https://` goes through `curl`; `file://`
/// is copied and only when a local preview was asked for.
fn fetch(url: &str, destination: &Path, max_bytes: u64, local_preview: bool) -> Result<()> {
    if let Some(path) = url.strip_prefix("file://") {
        ensure!(
            local_preview,
            "file:// feeds and archives are for local previews; pass --local-preview"
        );
        let source = Path::new(path);
        let size = source.symlink_metadata()?.len();
        ensure!(
            source.symlink_metadata()?.is_file() && size <= max_bytes,
            "local update source must be a regular file within {max_bytes} bytes"
        );
        std::fs::copy(source, destination)?;
        return Ok(());
    }
    ensure!(
        url.starts_with("https://"),
        "update sources must use https:// (got {url})"
    );
    let output = command::run(
        Path::new("/"),
        &curl_argv(url, destination, max_bytes),
        FETCH_TIMEOUT,
    )
    .context("cannot run curl to fetch the update")?;
    ensure!(
        output.success,
        "download of {url} failed: {}",
        output.text.trim()
    );
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Validate a feed and pick the release for this machine. A universal Mac
/// archive stands in when no architecture-specific one is listed.
fn select(feed: &Feed, target: &str, local_preview: bool) -> Result<FeedRelease> {
    ensure!(
        feed.format == 1 && feed.product == "agentdocker",
        "unsupported update feed"
    );
    ensure!(
        matches!(feed.channel.as_str(), "stable" | "preview"),
        "unknown feed channel {}",
        feed.channel
    );
    ensure!(
        feed.channel == "stable" || local_preview,
        "this is a preview feed; pass --local-preview to accept preview builds"
    );
    let release = feed
        .releases
        .iter()
        .find(|r| r.target == target)
        .or_else(|| {
            target
                .ends_with("apple-darwin")
                .then(|| {
                    feed.releases
                        .iter()
                        .find(|r| r.target == "universal-apple-darwin")
                })
                .flatten()
        })
        .with_context(|| format!("the feed lists no release for {target}"))?
        .clone();
    ensure!(
        release.archive.sha256.len() == 64
            && release
                .archive
                .sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
        "feed archive checksum is not a SHA-256"
    );
    ensure!(
        release.archive.bytes > 0 && release.archive.bytes <= ARCHIVE_MAX_BYTES,
        "feed archive size {} is outside the download budget",
        release.archive.bytes
    );
    let expected_name = format!(
        "agentdocker-desktop-{}.{}",
        release.target,
        if release.target.ends_with("apple-darwin") {
            "zip"
        } else {
            "tar.gz"
        }
    );
    ensure!(
        release.archive.name == expected_name,
        "feed names an unexpected archive {}",
        release.archive.name
    );
    ensure!(
        release.archive.url.starts_with("https://")
            || (local_preview && release.archive.url.starts_with("file://")),
        "feed archive URL must be https://"
    );
    ensure!(
        !release.version.is_empty()
            && release.version.len() <= 64
            && release
                .version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c)),
        "feed version is not a plain version string"
    );
    Ok(release)
}

/// How many agents the daemon reports live, without starting a daemon that
/// is not running. `None` when nothing answered.
fn live_agents(socket: Option<PathBuf>) -> Option<usize> {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        runtime.block_on(async {
            let client = crate::client::Client::new(socket).with_start_timeout(None);
            match client
                .call(&Request::List {
                    all: false,
                    project: None,
                    labels: BTreeMap::new(),
                })
                .await
            {
                Ok(Response::Agents { agents }) => Some(
                    agents
                        .iter()
                        .filter(|a| {
                            a.status.is_live() && a.spec.runtime != agentdocker_core::HUMAN_RUNTIME
                        })
                        .count(),
                ),
                _ => None,
            }
        })
    })
    .join()
    .ok()
    .flatten()
}

/// The entry names an archive listing tool printed, one per line.
fn listing(argv: &[String]) -> Result<Vec<String>> {
    let output = command::run(Path::new("/"), argv, FETCH_TIMEOUT)?;
    ensure!(
        output.success,
        "cannot list the archive: {}",
        output.text.trim()
    );
    Ok(output
        .stdout
        .lines()
        .map(str::to_owned)
        .filter(|l| !l.is_empty())
        .collect())
}

/// Refuse an entry path that could write outside the extraction directory or
/// outside the payload: absolute, empty, `..` or `.` components, or a top
/// level other than the payload (and, on macOS, ditto's `__MACOSX` sidecar).
fn audit_entry_name(name: &str, payload: &str) -> Result<()> {
    ensure!(!name.is_empty(), "archive has an unnamed entry");
    ensure!(
        !name.starts_with('/') && !name.contains('\\') && !name.contains('\0'),
        "archive entry {name:?} has an unsafe path"
    );
    for part in name.split('/') {
        ensure!(
            part != ".." && part != ".",
            "archive entry {name:?} escapes its directory"
        );
    }
    let top = name.split('/').next().unwrap_or("");
    ensure!(
        top == payload || (cfg!(target_os = "macos") && top == "__MACOSX"),
        "archive entry {name:?} is outside the {payload} payload"
    );
    Ok(())
}

/// Refuse link and special entries: the payload may not contain any, and a
/// link written first could redirect a later entry outside the destination.
fn audit_entry_modes(long_lines: &[String]) -> Result<()> {
    for line in long_lines {
        let kind = line.trim_start().chars().next().unwrap_or('-');
        ensure!(
            !matches!(kind, 'l' | 'b' | 'c' | 'p' | 's'),
            "archive contains a link or special file: {}",
            line.trim()
        );
    }
    Ok(())
}

/// Look inside the archive before anything is written from it.
fn audit_archive(archive: &Path, payload: &str) -> Result<()> {
    let path = archive.to_string_lossy().into_owned();
    let (names, long) = if cfg!(target_os = "macos") {
        (
            listing(&["/usr/bin/unzip".into(), "-Z1".into(), path.clone()])?,
            listing(&["/usr/bin/unzip".into(), "-Z".into(), path])?,
        )
    } else {
        (
            listing(&["tar".into(), "-tzf".into(), path.clone()])?,
            listing(&["tar".into(), "-tvzf".into(), path])?,
        )
    };
    ensure!(!names.is_empty(), "archive is empty");
    for name in &names {
        audit_entry_name(name, payload)?;
    }
    // zipinfo's long listing ends with summary lines that start with a digit.
    let modes: Vec<String> = long
        .into_iter()
        .filter(|l| l.starts_with(['-', 'd', 'l', 'b', 'c', 'p', 's']))
        .collect();
    audit_entry_modes(&modes)
}

fn extract(archive: &Path, destination: &Path) -> Result<PathBuf> {
    let payload_name = if cfg!(target_os = "macos") {
        "AgentDocker.app"
    } else {
        "agentdocker-desktop"
    };
    audit_archive(archive, payload_name)?;
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    dirs::ensure_private_dir(destination)?;
    let argv: Vec<String> = if cfg!(target_os = "macos") {
        vec![
            "/usr/bin/ditto".into(),
            "-x".into(),
            "-k".into(),
            archive.to_string_lossy().into_owned(),
            destination.to_string_lossy().into_owned(),
        ]
    } else {
        vec![
            "tar".into(),
            "-xzf".into(),
            archive.to_string_lossy().into_owned(),
            "-C".into(),
            destination.to_string_lossy().into_owned(),
        ]
    };
    let output = command::run(Path::new("/"), &argv, FETCH_TIMEOUT)?;
    ensure!(
        output.success,
        "cannot extract the update: {}",
        output.text.trim()
    );
    let payload = destination.join(payload_name);
    ensure!(
        payload.symlink_metadata()?.is_dir(),
        "the archive does not contain the desktop payload"
    );
    Ok(payload)
}

pub fn run(layout: &Layout, active: Option<&Activation>, options: Options) -> Result<()> {
    let target = host_target();
    // A check must leave no trace, so the feed lands in scratch; only a
    // download creates the private staging area beside the versions.
    let scratch = tempfile::tempdir()?;
    let feed_file = scratch.path().join("updates.json");
    fetch(
        &options.feed,
        &feed_file,
        FEED_MAX_BYTES,
        options.local_preview,
    )
    .context("cannot fetch the update feed")?;
    let feed: Feed = serde_json::from_slice(&std::fs::read(&feed_file)?)
        .context("the update feed is not valid JSON of the expected shape")?;
    let release = select(&feed, target, options.local_preview)?;
    let installed = active.map(|a| a.current.version.as_str());
    let running = env!("CARGO_PKG_VERSION");
    let baseline = installed.unwrap_or(running);
    let update_available = compare_versions(&release.version, baseline) == Ordering::Greater;
    let schema_change = active.is_some_and(|a| release.state_schema > a.current.state_schema);
    let live = live_agents(options.socket.clone());
    let guidance = match live {
        Some(0) => "no agents are live; `agentdocker daemon restart` switches the daemon now",
        Some(_) => "agents are live; finish their work before `agentdocker daemon restart`",
        None => "no daemon answered; the next launch starts the installed version",
    };
    let mut update = json!({
        "feed": options.feed,
        "channel": feed.channel,
        "policy": feed.policy,
        "target": target,
        "installed_version": installed,
        "running_version": running,
        "available": {
            "version": release.version,
            "source_commit": release.source_commit,
            "state_schema": release.state_schema,
            "signing": release.signing,
            "notarized": release.notarized,
            "archive": release.archive,
        },
        "update_available": update_available,
        "state_schema_change": schema_change,
        "live_agents": live,
        "daemon": guidance,
    });
    if options.check || !update_available {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"update": update}))?
        );
        return Ok(());
    }
    // Download once per version; a complete, matching archive is reused.
    layout.ensure_root()?;
    let staging = layout.root.join("downloads");
    dirs::ensure_private_dir(&staging)?;
    let version_dir = staging.join(&release.version);
    dirs::ensure_private_dir(&version_dir)?;
    let archive = version_dir.join(&release.archive.name);
    let matches = |path: &Path| -> Result<bool> {
        Ok(path.is_file()
            && path.metadata()?.len() == release.archive.bytes
            && file_sha256(path)? == release.archive.sha256)
    };
    if !matches(&archive)? {
        let partial = version_dir.join(format!("{}.part", release.archive.name));
        let _ = std::fs::remove_file(&partial);
        fetch(
            &release.archive.url,
            &partial,
            release.archive.bytes,
            options.local_preview,
        )
        .context("cannot download the update archive")?;
        if !matches(&partial)? {
            let _ = std::fs::remove_file(&partial);
            bail!(
                "downloaded archive does not match the feed (expected {} bytes, sha256 {}); nothing installed",
                release.archive.bytes,
                release.archive.sha256
            );
        }
        std::fs::rename(&partial, &archive)?;
    }
    update["downloaded"] = json!(archive);
    let extracted = version_dir.join("payload");
    // A payload that fails any check is not kept around: the next attempt
    // downloads and extracts afresh rather than trusting a rejected tree.
    let checked = (|| -> Result<(PathBuf, super::Release)> {
        let payload = extract(&archive, &extracted)?;
        // The installer has already refused links, special files, foreign
        // targets and bad signatures inside the payload; here the payload
        // must also be the very release the feed described, and never a
        // downgrade.
        let (source, candidate) = inspect(&payload, options.local_preview)?;
        ensure!(
            candidate.version == release.version
                && candidate.source_commit == release.source_commit
                && candidate.state_schema == release.state_schema
                && candidate.target == release.target,
            "the archive's payload does not match the feed entry (version, source, schema or target)"
        );
        ensure!(
            compare_versions(&candidate.version, baseline) == Ordering::Greater,
            "the payload is not newer than what is installed; nothing installed"
        );
        Ok((source, candidate))
    })();
    let (source, candidate) = match checked {
        Ok(found) => found,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&extracted);
            let _ = std::fs::remove_file(&archive);
            return Err(error);
        }
    };
    let expect_current = Some(active.map_or("none", |a| a.current.id.as_str()).to_owned());
    let mut report = perform(
        layout,
        active.cloned(),
        source,
        candidate.clone(),
        !options.apply,
        options.local_preview,
        Some(candidate.id.clone()),
        expect_current,
    )?;
    report["update"] = update;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(channel: &str, target: &str, url: &str) -> Feed {
        serde_json::from_value(json!({
            "format": 1, "product": "agentdocker", "channel": channel,
            "policy": {"download": "manual"},
            "releases": [{
                "target": target, "version": "0.2.0", "source_commit": "a".repeat(40),
                "state_schema": 10, "signing": "developer-id", "notarized": true,
                "archive": {"name": format!("agentdocker-desktop-{target}.{}", if target.ends_with("apple-darwin") {"zip"} else {"tar.gz"}),
                            "sha256": "b".repeat(64), "bytes": 12_345_678, "url": url}
            }]
        }))
        .unwrap()
    }

    #[test]
    fn archive_entries_may_not_escape_or_link() {
        let payload = if cfg!(target_os = "macos") {
            "AgentDocker.app"
        } else {
            "agentdocker-desktop"
        };
        assert!(audit_entry_name(&format!("{payload}/Contents/MacOS/agentd"), payload).is_ok());
        assert!(audit_entry_name(&format!("{payload}/"), payload).is_ok());
        for bad in [
            "/etc/passwd",
            "../x",
            &format!("{payload}/../../x"),
            "other/thing",
            "",
            &format!("{payload}/./x"),
            "C:\\x",
        ] {
            assert!(audit_entry_name(bad, payload).is_err(), "{bad:?} accepted");
        }
        assert!(
            audit_entry_modes(&[
                "-rwxr-xr-x  3.0 unx 10 bx defN 26-Sep-10 12:00 a".into(),
                "drwxr-xr-x 0 u g 0 Sep 10 12:00 d/".into()
            ])
            .is_ok()
        );
        assert!(
            audit_entry_modes(&["lrwxr-xr-x  3.0 unx 10 bx defN 26-Sep-10 12:00 a -> /etc".into()])
                .is_err()
        );
        assert!(audit_entry_modes(&["crw-rw-rw- 0 root wheel 0 Sep 10 12:00 dev".into()]).is_err());
    }

    #[test]
    fn versions_compare_like_semantic_versions() {
        assert_eq!(compare_versions("0.2.0", "0.1.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.10", "0.1.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0+build.7"), Ordering::Equal);
        assert_eq!(compare_versions("1.0.0-rc.1", "1.0.0"), Ordering::Less);
        assert_eq!(
            compare_versions("1.0.0-rc.2", "1.0.0-rc.1"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("0.1.0", "0.1.0"), Ordering::Equal);
    }

    #[test]
    fn the_feed_must_name_this_machine_or_a_universal_mac_build() {
        let host = host_target();
        let chosen = select(
            &feed("stable", host, "https://example.invalid/a.zip"),
            host,
            false,
        )
        .unwrap();
        assert_eq!(chosen.target, host);
        let other = if host.starts_with("aarch64") {
            "x86_64-apple-darwin"
        } else {
            "aarch64-apple-darwin"
        };
        assert!(
            select(
                &feed("stable", other, "https://example.invalid/a.zip"),
                host,
                false
            )
            .is_err()
        );
        if host.ends_with("apple-darwin") {
            let universal = select(
                &feed(
                    "stable",
                    "universal-apple-darwin",
                    "https://example.invalid/a.zip",
                ),
                host,
                false,
            )
            .unwrap();
            assert_eq!(universal.target, "universal-apple-darwin");
        }
    }

    #[test]
    fn preview_feeds_and_file_urls_need_an_explicit_local_preview() {
        let host = host_target();
        assert!(
            select(
                &feed("preview", host, "https://example.invalid/a.zip"),
                host,
                false
            )
            .is_err()
        );
        assert!(
            select(
                &feed("preview", host, "https://example.invalid/a.zip"),
                host,
                true
            )
            .is_ok()
        );
        assert!(select(&feed("stable", host, "file:///tmp/a.zip"), host, false).is_err());
        assert!(select(&feed("stable", host, "file:///tmp/a.zip"), host, true).is_ok());
        assert!(
            select(
                &feed("stable", host, "http://example.invalid/a.zip"),
                host,
                true
            )
            .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        assert!(fetch("file:///nonexistent", &dir.path().join("x"), 10, false).is_err());
        assert!(fetch("http://example.invalid/", &dir.path().join("x"), 10, true).is_err());
    }

    #[test]
    fn curl_is_pinned_to_https_with_bounded_size_and_time() {
        let argv = curl_argv(
            "https://example.invalid/updates.json",
            Path::new("/tmp/out"),
            4096,
        );
        let joined = argv.join(" ");
        for flag in [
            "--proto =https",
            "--proto-redir =https",
            "--tlsv1.2",
            "--max-filesize 4096",
            "--max-redirs 5",
            "--fail",
        ] {
            assert!(joined.contains(flag), "{flag} missing from {joined}");
        }
        assert_eq!(argv.last().unwrap(), "https://example.invalid/updates.json");
    }

    #[test]
    fn a_bad_checksum_or_oversized_archive_is_refused_before_any_download() {
        let host = host_target();
        let mut bad = feed("stable", host, "https://example.invalid/a.zip");
        bad.releases[0].archive.sha256 = "zz".repeat(32);
        assert!(select(&bad, host, false).is_err());
        let mut huge = feed("stable", host, "https://example.invalid/a.zip");
        huge.releases[0].archive.bytes = ARCHIVE_MAX_BYTES + 1;
        assert!(select(&huge, host, false).is_err());
        let mut renamed = feed("stable", host, "https://example.invalid/a.zip");
        renamed.releases[0].archive.name = "agentdocker.tar.gz".into();
        assert!(select(&renamed, host, false).is_err());
    }
}
