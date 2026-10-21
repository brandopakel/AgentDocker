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

/// How long a `tailscale` helper may take before it is killed and reaped.
const HELPER_DEADLINE: Duration = Duration::from_secs(15);
/// How long a dropped tunnel waits for its teardown, on whatever thread
/// drops it, before it kills the helper and goes on.
const DROP_DEADLINE: Duration = Duration::from_secs(3);

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
    /// Where this process recorded its Funnel, removed once it is undone.
    record: Option<PathBuf>,
}

/// A tunnel the connector did not stop — an error on the way up, a panic —
/// is taken down as it is dropped: a Funnel left behind keeps publishing a
/// loopback port that any later process could bind.
impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
        // Dropping may happen on a runtime thread, so the wait is short and
        // bounded: a helper that hangs is killed and reaped.
        if let Some((binary, args)) = self.teardown.take()
            && let Ok(mut child) = std::process::Command::new(&binary)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        {
            let deadline = std::time::Instant::now() + DROP_DEADLINE;
            while matches!(child.try_wait(), Ok(None)) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            match child.try_wait() {
                Ok(Some(status)) if status.success() => self.forget_record(),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!(
                        "agentdocker connector: `{}` did not finish; the Funnel stays recorded for the next start",
                        args.join(" ")
                    );
                }
                _ => eprintln!(
                    "agentdocker connector: `{}` failed; the Funnel stays recorded for the next start",
                    args.join(" ")
                ),
            }
        }
    }
}

/// What tailscaled's serve configuration points `https_port` at, as
/// `127.0.0.1:<port>` or whatever else it proxies to; `None` when nothing
/// is served there.
pub fn funnel_target(config: &serde_json::Value, https_port: u16) -> Option<String> {
    let suffix = format!(":{https_port}");
    config["Web"].as_object()?.iter().find_map(|(host, site)| {
        host.ends_with(&suffix).then(|| {
            let proxy = site["Handlers"]["/"]["Proxy"].as_str().unwrap_or("");
            let proxy = proxy.trim_end_matches('/');
            proxy.strip_prefix("http://").unwrap_or(proxy).to_owned()
        })
    })
}

/// What the Funnel on `https_port` points at now.
pub async fn funnel_target_now(binary: &Path, https_port: u16) -> Result<Option<String>, String> {
    let output = run_helper(binary, &["serve".into(), "status".into(), "--json".into()]).await?;
    let config: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("tailscale serve status is not JSON: {error}"))?;
    Ok(funnel_target(&config, https_port))
}

/// Clear the Funnel on `https_port`, and say whether it was cleared.
pub async fn clear_funnel(binary: &Path, https_port: u16) -> Result<(), String> {
    run_helper(binary, &funnel_off(https_port))
        .await
        .map(|_| ())
}

fn funnel_off(https_port: u16) -> Vec<String> {
    vec![
        "funnel".into(),
        format!("--https={https_port}"),
        "off".into(),
    ]
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

    /// End the child and undo the configuration. An error says the
    /// configuration may still be set, so whatever records it is kept.
    pub async fn stop(&mut self) -> Result<(), String> {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }
        match self.teardown.take() {
            Some((binary, args)) => {
                run_helper(&binary, &args).await?;
                self.forget_record();
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// The Funnel is confirmed undone: its record goes.
    fn forget_record(&mut self) {
        if let Some(home) = self.record.take() {
            agentdocker_host::connector::clear_funnel(&home, std::process::id());
        }
    }
}

/// Run a `tailscale` helper to the end, within `HELPER_DEADLINE`: one that
/// does not finish is killed and reaped, never left to act later.
async fn run_helper(binary: &Path, args: &[String]) -> Result<std::process::Output, String> {
    let child = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot run {}: {error}", binary.display()))?;
    match tokio::time::timeout(HELPER_DEADLINE, child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => Ok(output),
        Ok(Ok(output)) => Err(format!(
            "`{} {}` failed: {}",
            binary.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(Err(error)) => Err(format!("`{}` did not finish: {error}", args.join(" "))),
        // The future, and with it the child, is dropped: killed and reaped.
        Err(_) => Err(format!(
            "`{}` did not finish in {} s",
            args.join(" "),
            HELPER_DEADLINE.as_secs()
        )),
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
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let name = if cfg!(windows) {
        "tailscale.exe"
    } else {
        "tailscale"
    };
    let mut candidates: Vec<PathBuf> = agentdocker_host::command::find_program(&dirs, name)
        .into_iter()
        .collect();
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
///
/// The Funnel is recorded under `home` before it is set and the record is
/// kept until it is confirmed undone, so it is never open unrecorded. A
/// port that already serves something else is refused, never taken over,
/// and a set that fails is undone only where it took effect.
pub async fn funnel_tailscale(
    binary: &Path,
    port: u16,
    https_port: u16,
    home: &Path,
) -> Result<Tunnel> {
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
    let status = run_helper(binary, &["status".into(), "--json".into()])
        .await
        .map_err(|error| anyhow::anyhow!("tailscale status failed: {error}"))?;
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
    let target = format!("127.0.0.1:{port}");
    match funnel_target_now(binary, https_port).await {
        Ok(None) => {}
        Ok(Some(other)) => bail!(
            "Tailscale already serves {other} on port {https_port}; choose another --tunnel-port ({}), or clear it with `tailscale funnel --https={https_port} off` if it is not in use",
            FUNNEL_PORTS
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Err(error) => bail!("cannot read what Tailscale serves on port {https_port}: {error}"),
    }
    let pid = std::process::id();
    agentdocker_host::connector::write_funnel(
        home,
        &agentdocker_host::connector::FunnelRecord {
            pid,
            https_port,
            target: target.clone(),
        },
    )
    .context("cannot record the Funnel before setting it")?;
    let recorded = agentdocker_host::connector::funnel_path(home);
    if let Err(error) = run_helper(
        binary,
        &[
            "funnel".into(),
            "--bg".into(),
            format!("--https={https_port}"),
            format!("http://{target}"),
        ],
    )
    .await
    {
        // A set that ran over may have taken effect. Undo it only if the
        // port now points at this attempt's own address; anything else
        // there is somebody else's. Whatever cannot be confirmed stays
        // recorded for the next start.
        match funnel_target_now(binary, https_port).await {
            Ok(now) if now.as_deref() != Some(target.as_str()) => {
                agentdocker_host::connector::clear_funnel(home, pid);
                bail!("tailscale funnel refused: {error}");
            }
            Ok(_) => match clear_funnel(binary, https_port).await {
                Ok(()) => {
                    agentdocker_host::connector::clear_funnel(home, pid);
                    bail!("tailscale funnel refused: {error}");
                }
                Err(off) => bail!(
                    "tailscale funnel did not finish setting ({error}) and undoing it failed ({off}); {} keeps it for the next start to clear",
                    recorded.display()
                ),
            },
            Err(read) => bail!(
                "tailscale funnel did not finish setting ({error}) and whether it took effect cannot be read ({read}); {} keeps it for the next start to clear",
                recorded.display()
            ),
        }
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
        teardown: Some((binary.to_owned(), funnel_off(https_port))),
        record: Some(home.to_owned()),
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
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let name = if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    };
    let mut candidates: Vec<PathBuf> = agentdocker_host::command::find_program(&dirs, name)
        .into_iter()
        .collect();
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
        record: None,
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

    /// How a fake tailscale behaves: whether `funnel --bg` takes effect and
    /// whether it then fails, whether `off` fails, and what the port serves
    /// to begin with (`"<port> <proxy>"`).
    #[derive(Default)]
    struct Behaviour {
        set_without_effect: bool,
        set_fails: bool,
        off_fails: bool,
        serving: Option<&'static str>,
    }

    /// A fake tailscale that keeps its serve configuration in a file beside
    /// it and records every command it is given in `calls.log`.
    #[cfg(unix)]
    fn stateful_tailscale(dir: &Path, behaviour: Behaviour) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let state = dir.join("serving");
        std::fs::write(&state, behaviour.serving.unwrap_or("")).unwrap();
        let script = r#"#!/bin/sh
echo "$*" >> @LOG@
case "$1" in
status) echo '{"Self":{"DNSName":"mac.tail1.ts.net.","Online":true,"CapMap":{"funnel":[],"https":[]}}}' ;;
serve)
  t=$(cat @STATE@)
  if [ -n "$t" ]; then
    printf '{"Web":{"mac.tail1.ts.net:%s":{"Handlers":{"/":{"Proxy":"%s"}}}}}\n' "${t%% *}" "${t#* }"
  else
    echo '{}'
  fi ;;
funnel)
  if [ "$3" = off ]; then
    @OFF@
    : > @STATE@
  else
    @SET@
    @SETFAIL@
  fi ;;
esac
exit 0
"#
        .replace("@LOG@", &dir.join("calls.log").display().to_string())
        .replace("@STATE@", &state.display().to_string())
        .replace(
            "@OFF@",
            if behaviour.off_fails {
                "echo 'tailscaled is not answering' >&2; exit 1"
            } else {
                ":"
            },
        )
        .replace(
            "@SET@",
            if behaviour.set_without_effect {
                ":"
            } else {
                r#"echo "${3#--https=} $4" > @STATE@"#
            },
        )
        .replace("@STATE@", &state.display().to_string())
        .replace(
            "@SETFAIL@",
            if behaviour.set_fails {
                "echo 'the funnel did not answer' >&2; exit 1"
            } else {
                ":"
            },
        );
        let fake = dir.join("tailscale");
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    /// The public URL is the node's stable name; the Funnel is recorded
    /// before it is set, set on the bind port, and cleared, with its
    /// record, on stop or when dropped unstopped; a stopped one is not
    /// cleared twice.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_funnel_is_the_nodes_own_name_set_on_entry_and_cleared_on_exit() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let fake = stateful_tailscale(dir.path(), Behaviour::default());
        let calls = || std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        let mut tunnel = funnel_tailscale(&fake, 8765, 443, home.path())
            .await
            .unwrap();
        assert_eq!(tunnel.public_url, "https://mac.tail1.ts.net");
        assert_eq!(tunnel.name.as_deref(), Some("mac.tail1.ts.net"));
        assert!(tunnel.pid().is_none() && tunnel.exited().is_none());
        assert_eq!(
            agentdocker_host::connector::read_funnel(home.path()).map(|r| r.target),
            Some("127.0.0.1:8765".to_owned()),
            "recorded while it is set"
        );
        tunnel.stop().await.unwrap();
        assert!(
            calls().contains("funnel --bg --https=443 http://127.0.0.1:8765"),
            "{}",
            calls()
        );
        assert!(calls().contains("funnel --https=443 off"), "{}", calls());
        assert!(agentdocker_host::connector::read_funnel(home.path()).is_none());
        drop(tunnel);
        let other = funnel_tailscale(&fake, 8765, 8443, home.path())
            .await
            .unwrap();
        assert_eq!(other.public_url, "https://mac.tail1.ts.net:8443");
        drop(other);
        assert!(agentdocker_host::connector::read_funnel(home.path()).is_none());
        assert!(
            funnel_tailscale(&fake, 8765, 8080, home.path())
                .await
                .is_err()
        );
        clear_funnel(&fake, 10000).await.unwrap();
        assert_eq!(
            calls().matches("funnel --https=443 off").count(),
            1,
            "{}",
            calls()
        );
        assert_eq!(
            calls().matches("funnel --https=8443 off").count(),
            1,
            "{}",
            calls()
        );
        assert!(calls().contains("funnel --https=10000 off"), "{}", calls());

        use std::os::unix::fs::PermissionsExt;
        let home = home.path();
        let unable = dir.path().join("unable");
        std::fs::write(
            &unable,
            "#!/bin/sh\nif [ \"$1\" = status ]; then echo '{\"Self\":{\"DNSName\":\"mac.tail1.ts.net.\",\"Online\":true,\"CapMap\":{\"https\":[]}}}'; fi\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&unable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = funnel_tailscale(&unable, 8765, 443, home)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Funnel is not enabled"), "{error}");
        assert!(find_tailscale(Some(&fake)).is_ok());
    }

    /// A port that serves somebody else's address is refused before
    /// anything is set, and nothing of it is undone.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_port_another_service_holds_is_never_taken_or_undone() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let fake = stateful_tailscale(
            dir.path(),
            Behaviour {
                serving: Some("443 http://127.0.0.1:3000"),
                ..Behaviour::default()
            },
        );
        let error = funnel_tailscale(&fake, 8765, 443, home.path())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("already serves 127.0.0.1:3000"), "{error}");
        let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
        assert!(!calls.contains("--bg") && !calls.contains("off"), "{calls}");
        assert!(agentdocker_host::connector::read_funnel(home.path()).is_none());
    }

    /// A set that fails is undone only where it took effect, and kept on
    /// record while that cannot be confirmed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_set_is_undone_only_where_it_took_effect() {
        let attempt = |behaviour: Behaviour| async move {
            let dir = tempfile::tempdir().unwrap();
            let home = tempfile::tempdir().unwrap();
            let fake = stateful_tailscale(dir.path(), behaviour);
            let error = funnel_tailscale(&fake, 8765, 443, home.path())
                .await
                .unwrap_err()
                .to_string();
            let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
            let recorded = agentdocker_host::connector::read_funnel(home.path());
            (error, calls, recorded)
        };
        // Nothing took effect: nothing to undo, nothing kept.
        let (error, calls, recorded) = attempt(Behaviour {
            set_without_effect: true,
            set_fails: true,
            ..Behaviour::default()
        })
        .await;
        assert!(error.contains("refused"), "{error}");
        assert!(!calls.contains("off"), "{calls}");
        assert!(recorded.is_none());
        // It took effect: undone, and its record goes.
        let (error, calls, recorded) = attempt(Behaviour {
            set_fails: true,
            ..Behaviour::default()
        })
        .await;
        assert!(error.contains("refused"), "{error}");
        assert_eq!(
            calls.matches("funnel --https=443 off").count(),
            1,
            "{calls}"
        );
        assert!(recorded.is_none());
        // It took effect and undoing it failed: kept on record.
        let (error, _, recorded) = attempt(Behaviour {
            set_fails: true,
            off_fails: true,
            ..Behaviour::default()
        })
        .await;
        assert!(error.contains("keeps it for the next start"), "{error}");
        assert_eq!(recorded.map(|r| r.https_port), Some(443));
    }

    /// A Funnel that cannot be cleared on the way out says so and stays on
    /// record for the next start.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_teardown_is_reported_not_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let fake = stateful_tailscale(
            dir.path(),
            Behaviour {
                off_fails: true,
                ..Behaviour::default()
            },
        );
        let mut tunnel = funnel_tailscale(&fake, 8765, 443, home.path())
            .await
            .unwrap();
        let error = tunnel.stop().await.unwrap_err();
        assert!(error.contains("tailscaled is not answering"), "{error}");
        assert!(agentdocker_host::connector::read_funnel(home.path()).is_some());
    }

    #[test]
    fn a_funnel_is_a_connectors_only_while_it_serves_its_address() {
        let config: serde_json::Value = serde_json::from_str(
            r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"mac.tail1.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:8765"}}}},"AllowFunnel":{"mac.tail1.ts.net:443":true}}"#,
        )
        .unwrap();
        assert_eq!(
            funnel_target(&config, 443).as_deref(),
            Some("127.0.0.1:8765")
        );
        assert_eq!(
            funnel_target(&config, 8443),
            None,
            "another port serves nothing"
        );
        assert_eq!(funnel_target(&serde_json::json!({}), 443), None);
    }

    /// A teardown that never finishes does not hold up whoever drops the
    /// tunnel: it is killed after a short wait.
    #[cfg(unix)]
    #[test]
    fn a_dropped_tunnel_never_waits_long_on_its_teardown() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let hang = dir.path().join("tailscale");
        std::fs::write(&hang, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tunnel = Tunnel {
            provider: "tailscale",
            public_url: "https://mac.tail1.ts.net".into(),
            name: None,
            child: None,
            teardown: Some((hang, funnel_off(443))),
            record: None,
        };
        let started = std::time::Instant::now();
        drop(tunnel);
        assert!(
            started.elapsed() < DROP_DEADLINE + Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_quick_tunnel_url_is_read_out_of_cloudflareds_box() {
        let line = "2026-01-02T03:04:05Z INF |  https://calm-river-sample-words.trycloudflare.com                                     |";
        assert_eq!(
            quick_tunnel_url(line).as_deref(),
            Some("https://calm-river-sample-words.trycloudflare.com")
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
        tunnel.stop().await.unwrap();
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
        let mut tunnel = spawn_cloudflared(
            &named,
            6111,
            Some("orchard"),
            Some("https://ad.example.com"),
        )
        .await
        .unwrap();
        assert_eq!(tunnel.public_url, "https://ad.example.com");
        assert_eq!(tunnel.name.as_deref(), Some("orchard"));
        tunnel.stop().await.unwrap();
        let error = spawn_cloudflared(&named, 6111, Some("orchard"), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs --public-url"), "{error}");
    }
}
