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

/// A tunnel in front of the connector and the public URL it serves: a
/// child process of ours (cloudflared), or a configuration in a daemon
/// that outlives us (Tailscale Funnel), taken down when we stop.
#[derive(Debug)]
pub struct Tunnel {
    pub provider: &'static str,
    pub public_url: String,
    pub name: Option<String>,
    child: Option<Child>,
    /// The command that undoes the configuration, when there is one.
    teardown: Option<(PathBuf, Vec<String>)>,
}

impl Tunnel {
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    /// Whether the child has exited; the connector stops with it. A
    /// tunnel that is a daemon's configuration has no child to lose.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
    }

    pub async fn stop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }
        if let Some((binary, args)) = self.teardown.take() {
            let _ = tokio::time::timeout(
                Duration::from_secs(15),
                Command::new(binary)
                    .args(args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status(),
            )
            .await;
        }
    }
}

/// Where `tailscale` is: an explicit path, PATH, the usual places, or
/// the macOS app bundle's own CLI.
pub fn find_tailscale(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_owned());
        }
        bail!("{} is not a file", path.display());
    }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("tailscale"))
                .collect()
        })
        .unwrap_or_default();
    candidates.extend(
        [
            "/usr/local/bin/tailscale",
            "/opt/homebrew/bin/tailscale",
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/usr/bin/tailscale",
        ]
        .iter()
        .map(PathBuf::from),
    );
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .context("tailscale is not installed; see https://tailscale.com/download, then run again")
}

/// The ports Tailscale Funnel will serve on.
pub const FUNNEL_PORTS: &[u16] = &[443, 8443, 10000];

/// Expose the connector through Tailscale Funnel: the node's own
/// `*.ts.net` name, which is the same after every restart, on one of the
/// ports Funnel allows. Funnel is a configuration in tailscaled, not a
/// process of ours, so it is set on the way in and cleared on the way out.
pub async fn funnel_tailscale(binary: &Path, port: u16, https_port: u16) -> Result<Tunnel> {
    if !FUNNEL_PORTS.contains(&https_port) {
        bail!(
            "Tailscale Funnel serves on {} only; --tunnel-port {https_port} is not one of them",
            FUNNEL_PORTS
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let status = Command::new(binary)
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("cannot run {} status", binary.display()))?;
    if !status.status.success() {
        bail!(
            "tailscale status failed: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        );
    }
    let status: serde_json::Value =
        serde_json::from_slice(&status.stdout).context("tailscale status is not JSON")?;
    let this = &status["Self"];
    if this["Online"] != true {
        bail!("this machine is not online on its tailnet; `tailscale up` first");
    }
    let host = this["DNSName"]
        .as_str()
        .map(|name| name.trim_end_matches('.'))
        .filter(|name| !name.is_empty())
        .context("tailscale status names no DNS name for this machine; MagicDNS must be on")?
        .to_owned();
    let capable = this["CapMap"]
        .as_object()
        .is_some_and(|caps| caps.contains_key("funnel"));
    if !capable {
        bail!(
            "Funnel is not enabled for this machine: turn it on for the tailnet in the admin console (Access controls › nodeAttrs `funnel`), or run `{} funnel --bg {port}` once and follow the link it prints",
            binary.display()
        );
    }
    let target = format!("http://127.0.0.1:{port}");
    let https = format!("--https={https_port}");
    let set = Command::new(binary)
        .args(["funnel", "--bg", &https, &target])
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("cannot run {} funnel", binary.display()))?;
    if !set.status.success() {
        bail!(
            "tailscale funnel refused: {}{}",
            String::from_utf8_lossy(&set.stderr).trim(),
            String::from_utf8_lossy(&set.stdout).trim()
        );
    }
    let public_url = if https_port == 443 {
        format!("https://{host}")
    } else {
        format!("https://{host}:{https_port}")
    };
    Ok(Tunnel {
        provider: "tailscale",
        public_url,
        name: Some(host),
        child: None,
        teardown: Some((
            binary.to_owned(),
            vec!["funnel".into(), https, "off".into()],
        )),
    })
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
        child: Some(child),
        teardown: None,
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

    /// A fake tailscale answers `status --json` and records the funnel
    /// commands it is given: the public URL is the node's stable name,
    /// the funnel is set on the bind port and cleared on stop, and a
    /// machine without the capability is told what to enable.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_funnel_is_the_nodes_own_name_set_on_entry_and_cleared_on_exit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls.log");
        let fake = dir.path().join("tailscale");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$*\" >> {log}\nif [ \"$1\" = status ]; then echo '{{\"Self\":{{\"DNSName\":\"mac.tail1.ts.net.\",\"Online\":true,\"CapMap\":{{\"funnel\":[],\"https\":[]}}}}}}'; fi\nexit 0\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut tunnel = funnel_tailscale(&fake, 62800, 443).await.unwrap();
        assert_eq!(tunnel.public_url, "https://mac.tail1.ts.net");
        assert_eq!(tunnel.name.as_deref(), Some("mac.tail1.ts.net"));
        assert!(tunnel.pid().is_none() && tunnel.exited().is_none());
        tunnel.stop().await;
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("funnel --bg --https=443 http://127.0.0.1:62800"),
            "{calls}"
        );
        assert!(calls.contains("funnel --https=443 off"), "{calls}");
        let other = funnel_tailscale(&fake, 62800, 8443).await.unwrap();
        assert_eq!(other.public_url, "https://mac.tail1.ts.net:8443");
        assert!(funnel_tailscale(&fake, 62800, 8080).await.is_err());

        let unable = dir.path().join("unable");
        std::fs::write(
            &unable,
            "#!/bin/sh\nif [ \"$1\" = status ]; then echo '{\"Self\":{\"DNSName\":\"mac.tail1.ts.net.\",\"Online\":true,\"CapMap\":{\"https\":[]}}}'; fi\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&unable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = funnel_tailscale(&unable, 62800, 443)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Funnel is not enabled"), "{error}");
        assert!(find_tailscale(Some(&fake)).is_ok());
    }

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
