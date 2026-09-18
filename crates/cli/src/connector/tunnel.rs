//! The tunnel in front of the connector, run as a child when asked: a
//! cloudflared quick tunnel, whose random `*.trycloudflare.com` hostname
//! is read from its output, or a named tunnel the person has already
//! routed to a hostname of their own. The connector never speaks TLS
//! itself; the tunnel does, and it is what gives loopback a public name.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// How long cloudflared may take to register its connection.
const READY_DEADLINE: Duration = Duration::from_secs(60);
/// After the tunnel registers, its hostname is published in DNS a moment
/// later; asking a resolver before that makes it cache the absence for
/// the negative TTL (half an hour with an ISP resolver). So the URL is
/// held back this long before anyone is told it.
const DNS_SETTLE: Duration = Duration::from_secs(4);

/// A running tunnel child and the public URL it serves.
#[derive(Debug)]
pub struct Tunnel {
    pub provider: &'static str,
    pub public_url: String,
    pub name: Option<String>,
    child: Child,
}

impl Tunnel {
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Whether the child has exited; the connector stops with it.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    pub async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }
}

/// Where `cloudflared` is, from an explicit path or the usual places;
/// launchd starts services with a PATH that has neither Homebrew nor
/// `/usr/local/bin`, so the search is not left to the environment.
pub fn find_cloudflared(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_owned());
        }
        bail!("{} is not a file", path.display());
    }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("cloudflared"))
                .collect()
        })
        .unwrap_or_default();
    candidates.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
            .iter()
            .map(|dir| Path::new(dir).join("cloudflared")),
    );
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .context("cloudflared is not installed; `brew install cloudflared` (macOS) or see https://github.com/cloudflare/cloudflared, then run again")
}

/// Start cloudflared for `port`: a quick tunnel when `name` is none, whose
/// URL is read from the output; a named tunnel otherwise, which is up
/// once it reports a registered connection and whose URL is the one the
/// person routed to it.
pub async fn spawn_cloudflared(
    binary: &Path,
    port: u16,
    name: Option<&str>,
    public_url: Option<&str>,
) -> Result<Tunnel> {
    let local = format!("http://127.0.0.1:{port}");
    let mut command = Command::new(binary);
    command.arg("tunnel");
    match name {
        Some(name) => {
            command.args(["run", "--url", &local, name]);
        }
        None => {
            command.args(["--url", &local]);
        }
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot start {}", binary.display()))?;
    let stdout = child.stdout.take().context("cloudflared stdout")?;
    let stderr = child.stderr.take().context("cloudflared stderr")?;
    let (lines_tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    let tx = lines_tx.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    let wanted = name.is_none();
    let found = tokio::time::timeout(READY_DEADLINE, async {
        let mut url: Option<String> = None;
        let mut registered = false;
        while let Some(line) = lines.recv().await {
            if let Some(quick) = quick_tunnel_url(&line) {
                url = Some(quick);
            }
            if line.contains("Registered tunnel connection") {
                registered = true;
            }
            if registered && (!wanted || url.is_some()) {
                return Ok::<_, anyhow::Error>(url);
            }
            if line.contains("ERR") && (line.contains("failed") || line.contains("error")) {
                eprintln!("cloudflared: {line}");
            }
        }
        bail!("cloudflared exited before the tunnel was up")
    })
    .await;
    let url = match found {
        Ok(Ok(url)) => url,
        Ok(Err(error)) => {
            let _ = child.start_kill();
            return Err(error);
        }
        Err(_) => {
            let _ = child.start_kill();
            bail!("cloudflared did not bring the tunnel up within a minute");
        }
    };
    // Keep the child's output flowing so it never blocks on a full pipe.
    tokio::spawn(async move { while lines.recv().await.is_some() {} });
    let public_url = match (name, url, public_url) {
        (None, Some(url), _) => url,
        (Some(_), _, Some(url)) => url.to_owned(),
        (Some(_), _, None) => {
            let _ = child.start_kill();
            bail!("a named tunnel needs --public-url: the hostname you routed to it");
        }
        (None, None, _) => {
            let _ = child.start_kill();
            bail!("cloudflared registered but printed no trycloudflare.com URL");
        }
    };
    tokio::time::sleep(DNS_SETTLE).await;
    Ok(Tunnel {
        provider: "cloudflared",
        public_url,
        name: name.map(str::to_owned),
        child,
    })
}

/// The quick tunnel's hostname, as cloudflared prints it inside a box of
/// `|` characters and spaces.
pub fn quick_tunnel_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|')
        .unwrap_or(rest.len());
    let url = &rest[..end];
    url.ends_with(".trycloudflare.com").then(|| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quick_tunnel_url_is_read_out_of_cloudflareds_box() {
        let line = "2026-09-18T00:05:46Z INF |  https://safe-psp-generating-finding.trycloudflare.com                                     |";
        assert_eq!(
            quick_tunnel_url(line).as_deref(),
            Some("https://safe-psp-generating-finding.trycloudflare.com")
        );
        assert_eq!(
            quick_tunnel_url("INF Registered tunnel connection connIndex=0"),
            None
        );
        assert_eq!(
            quick_tunnel_url("visit https://example.com/x for docs"),
            None
        );
    }

    /// A fake cloudflared on disk: the connector reads the URL from the
    /// box and waits for the registration line; a fake that exits first
    /// is an error, not a hang.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_quick_tunnel_is_up_when_its_url_and_registration_are_printed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("cloudflared");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'INF Thank you for trying Cloudflare Tunnel' >&2\nsleep 0.1\necho 'INF |  https://fake-words-here.trycloudflare.com  |' >&2\necho 'INF Registered tunnel connection connIndex=0' >&2\nsleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let mut tunnel = spawn_cloudflared(&fake, 6111, None, None).await.unwrap();
        assert_eq!(
            tunnel.public_url,
            "https://fake-words-here.trycloudflare.com"
        );
        assert!(tunnel.pid().is_some());
        assert!(
            started.elapsed() >= DNS_SETTLE,
            "the URL is held back until DNS can have it"
        );
        assert!(tunnel.exited().is_none());
        tunnel.stop().await;
        assert!(find_cloudflared(Some(&fake)).is_ok());
        assert!(find_cloudflared(Some(&dir.path().join("missing"))).is_err());

        let dying = dir.path().join("dying");
        std::fs::write(
            &dying,
            "#!/bin/sh\necho 'ERR failed to connect' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&dying, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = spawn_cloudflared(&dying, 6111, None, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("exited before"), "{error}");

        let named = dir.path().join("named");
        std::fs::write(
            &named,
            "#!/bin/sh\necho \"args: $*\" >&2\necho 'INF Registered tunnel connection connIndex=0' >&2\nsleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&named, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut tunnel =
            spawn_cloudflared(&named, 6111, Some("keel"), Some("https://ad.example.com"))
                .await
                .unwrap();
        assert_eq!(tunnel.public_url, "https://ad.example.com");
        assert_eq!(tunnel.name.as_deref(), Some("keel"));
        tunnel.stop().await;
        let error = spawn_cloudflared(&named, 6111, Some("keel"), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs --public-url"), "{error}");
    }
}
