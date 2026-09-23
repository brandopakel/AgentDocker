//! Who may reach the vendor-facing endpoints, by the client address the
//! tunnel reports. Behind a tunnel every TCP peer is the tunnel itself, so
//! the address worth checking is the one the tunnel writes into a header
//! (`cf-connecting-ip` for cloudflared). The vendors publish their egress
//! ranges: Anthropic's is one block; OpenAI's is a JSON feed of a few
//! hundred prefixes that changes. The `openai` preset fetches that fixed
//! HTTPS feed at startup and hourly; an explicit file remains local-only.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

use super::http::Request;

/// Anthropic's published outbound range for connectors, from its
/// connector documentation. Stable enough to name; still one line to
/// change.
pub const ANTHROPIC_EGRESS: &[&str] = &["160.79.104.0/21"];
const OPENAI_FEED: &str = "https://openai.com/chatgpt-connectors.json";
const MAX_FEED_BYTES: usize = 256 * 1024;
const MAX_FEED_PREFIXES: usize = 4096;
const FETCH_DEADLINE: Duration = Duration::from_secs(10);
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Parse every entry before replacing the active list. Unknown, empty,
/// ambiguous or catch-all entries must not silently broaden admission.
fn parse_openai_feed(text: &str) -> Result<Vec<Cidr>> {
    if text.len() > MAX_FEED_BYTES {
        bail!("OpenAI egress feed exceeds 256 KiB");
    }
    let feed: serde_json::Value =
        serde_json::from_str(text).context("invalid OpenAI egress JSON")?;
    let entries = feed["prefixes"]
        .as_array()
        .context("missing OpenAI prefixes")?;
    if entries.is_empty() || entries.len() > MAX_FEED_PREFIXES {
        bail!("OpenAI egress feed must contain 1..={MAX_FEED_PREFIXES} prefixes");
    }
    entries
        .iter()
        .map(|entry| {
            let (text, v4) = match (entry.get("ipv4Prefix"), entry.get("ipv6Prefix")) {
                (Some(value), None) => (value.as_str().context("invalid IPv4 prefix")?, true),
                (None, Some(value)) => (value.as_str().context("invalid IPv6 prefix")?, false),
                _ => bail!("OpenAI entry must name exactly one IP prefix"),
            };
            let cidr = Cidr::parse(text)?;
            if !text.contains('/') || cidr.address.is_ipv4() != v4 || cidr.prefix == 0 {
                bail!("OpenAI entry has an invalid family or catch-all prefix");
            }
            Ok(cidr)
        })
        .collect()
}

fn fetch_openai_feed() -> Result<Vec<Cidr>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_DEADLINE))
        .max_redirects(0)
        .https_only(true)
        .http_status_as_error(false)
        .user_agent(concat!("agentdocker/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut response = agent
        .get(OPENAI_FEED)
        .header("Accept", "application/json")
        .call()
        .context("could not fetch OpenAI egress feed")?;
    if response.status().as_u16() != 200 {
        bail!("OpenAI egress feed returned HTTP {}", response.status());
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(MAX_FEED_BYTES as u64)
        .read_to_string()
        .context("could not read bounded OpenAI egress feed")?;
    parse_openai_feed(&text)
}

/// An IPv4 or IPv6 prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    address: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (
                address,
                Some(
                    prefix
                        .parse::<u8>()
                        .with_context(|| format!("{text}: the prefix length is not a number"))?,
                ),
            ),
            None => (text, None),
        };
        let address: IpAddr = address
            .parse()
            .with_context(|| format!("{text}: not an IP address"))?;
        let bits = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix = prefix.unwrap_or(bits);
        if prefix > bits {
            bail!("{text}: the prefix length exceeds {bits}");
        }
        Ok(Self { address, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.address, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - u32::from(self.prefix))
                };
                (u32::from(net) & mask) == (u32::from(ip) & mask)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - u32::from(self.prefix))
                };
                (u128::from(net) & mask) == (u128::from(ip) & mask)
            }
            // A v4 address arriving as a mapped v6 one is the v4 address.
            (IpAddr::V4(_), IpAddr::V6(ip)) => ip
                .to_ipv4_mapped()
                .is_some_and(|ip| self.contains(IpAddr::V4(ip))),
            (IpAddr::V6(_), IpAddr::V4(ip)) => self.contains(IpAddr::V6(ip.to_ipv6_mapped())),
        }
    }
}

/// Prefixes from a file: OpenAI's feed shape (`prefixes[].ipv4Prefix` /
/// `ipv6Prefix`), or one CIDR per line with `#` comments.
pub fn parse_prefix_file(text: &str) -> Result<Vec<Cidr>> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') {
        let feed: serde_json::Value =
            serde_json::from_str(trimmed).context("the prefix file is not valid JSON")?;
        let prefixes = feed["prefixes"]
            .as_array()
            .context("the JSON prefix file has no `prefixes` array")?;
        return prefixes
            .iter()
            .filter_map(|entry| {
                entry["ipv4Prefix"]
                    .as_str()
                    .or_else(|| entry["ipv6Prefix"].as_str())
            })
            .map(Cidr::parse)
            .collect();
    }
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .map(Cidr::parse)
        .collect()
}

struct FromFile {
    path: PathBuf,
    modified: Option<SystemTime>,
    cidrs: Vec<Cidr>,
}

/// Literal prefixes, vendor presets, and files re-read when they change.
/// Automatic OpenAI updates replace one validated snapshot at a time.
pub struct Allowlist {
    header: String,
    fixed: Vec<Cidr>,
    files: Mutex<Vec<FromFile>>,
    openai: Option<Mutex<Vec<Cidr>>>,
}

impl Allowlist {
    /// `values` are CIDRs, `anthropic`, `openai`, or `@<path>`. `header` names the
    /// header the tunnel writes the client address into.
    pub fn parse(values: &[String], header: &str) -> Result<Option<Self>> {
        if values.is_empty() {
            return Ok(None);
        }
        if header.is_empty() {
            bail!(
                "--allow-from needs the header the tunnel writes the client address into; pass --client-ip-header (cloudflared: cf-connecting-ip)"
            );
        }
        let mut fixed = Vec::new();
        let mut files = Vec::new();
        let mut openai = None;
        for value in values {
            if value == "openai" {
                // Before the required initial fetch there are no admitted
                // OpenAI addresses, never an implicit allow-all fallback.
                openai = Some(Mutex::new(Vec::new()));
            } else if value == "anthropic" {
                fixed.extend(
                    ANTHROPIC_EGRESS
                        .iter()
                        .map(|c| Cidr::parse(c).expect("constant")),
                );
            } else if let Some(path) = value.strip_prefix('@') {
                let path = PathBuf::from(path);
                let (modified, cidrs) = read_prefix_file(&path)?;
                files.push(FromFile {
                    path,
                    modified,
                    cidrs,
                });
            } else {
                fixed.push(Cidr::parse(value)?);
            }
        }
        Ok(Some(Self {
            header: header.to_ascii_lowercase(),
            fixed,
            files: Mutex::new(files),
            openai,
        }))
    }

    /// How many prefixes are in force right now.
    pub fn len(&self) -> usize {
        self.fixed.len()
            + self
                .files
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|f| f.cidrs.len())
                .sum::<usize>()
            + self
                .openai
                .as_ref()
                .map(|prefixes| prefixes.lock().unwrap_or_else(|e| e.into_inner()).len())
                .unwrap_or(0)
    }

    pub fn auto_refresh(&self) -> bool {
        self.openai.is_some()
    }

    pub async fn refresh_openai(&self) -> Result<()> {
        self.refresh_with(fetch_openai_feed).await
    }

    async fn refresh_with<F>(&self, fetch: F) -> Result<()>
    where
        F: FnOnce() -> Result<Vec<Cidr>> + Send + 'static,
    {
        let Some(prefixes) = &self.openai else {
            return Ok(());
        };
        // Network I/O never holds the admission lock or blocks the server's
        // async executor. A failure preserves the entire last good snapshot.
        let fresh = tokio::task::spawn_blocking(fetch)
            .await
            .context("OpenAI refresh worker failed")??;
        *prefixes.lock().unwrap_or_else(|e| e.into_inner()) = fresh;
        Ok(())
    }

    /// The client address the tunnel reports, when the header is there
    /// and parses; `x-forwarded-for` lists hops, the first is the client.
    pub fn client_ip(&self, request: &Request) -> Option<IpAddr> {
        let value = request.header(&self.header)?;
        value.split(',').next()?.trim().parse().ok()
    }

    /// Whether this request may reach a vendor-facing endpoint. No
    /// header, or an unparsable one, is a refusal: the allowlist exists
    /// to be exact.
    pub fn allows(&self, request: &Request) -> bool {
        let Some(ip) = self.client_ip(request) else {
            return false;
        };
        if self.fixed.iter().any(|c| c.contains(ip)) {
            return true;
        }
        if self.openai.as_ref().is_some_and(|prefixes| {
            prefixes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|c| c.contains(ip))
        }) {
            return true;
        }
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        for file in files.iter_mut() {
            let modified = std::fs::metadata(&file.path)
                .and_then(|m| m.modified())
                .ok();
            if modified != file.modified
                && let Ok((modified, cidrs)) = read_prefix_file(&file.path)
            {
                file.modified = modified;
                file.cidrs = cidrs;
            }
            if file.cidrs.iter().any(|c| c.contains(ip)) {
                return true;
            }
        }
        false
    }
}

fn read_prefix_file(path: &Path) -> Result<(Option<SystemTime>, Vec<Cidr>)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the prefix file {}", path.display()))?;
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let cidrs = parse_prefix_file(&text)
        .with_context(|| format!("{} is not a prefix file", path.display()))?;
    Ok((modified, cidrs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_from(header: &str, value: &str) -> Request {
        Request {
            method: "POST".into(),
            target: "/mcp".into(),
            headers: vec![(header.into(), value.into())],
            body: vec![],
        }
    }

    #[test]
    fn prefixes_contain_what_they_should_in_both_families() {
        let anthropic = Cidr::parse("160.79.104.0/21").unwrap();
        assert!(anthropic.contains("160.79.104.1".parse().unwrap()));
        assert!(anthropic.contains("160.79.111.255".parse().unwrap()));
        assert!(!anthropic.contains("160.79.112.0".parse().unwrap()));
        assert!(
            anthropic.contains("::ffff:160.79.105.9".parse().unwrap()),
            "mapped v4"
        );
        let single = Cidr::parse("100.31.168.162").unwrap();
        assert!(single.contains("100.31.168.162".parse().unwrap()));
        assert!(!single.contains("100.31.168.163".parse().unwrap()));
        let six = Cidr::parse("2001:db8::/32").unwrap();
        assert!(six.contains("2001:db8:1::1".parse().unwrap()));
        assert!(!six.contains("2001:db9::1".parse().unwrap()));
        assert!(!six.contains("10.0.0.1".parse().unwrap()));
        assert!(
            Cidr::parse("0.0.0.0/0")
                .unwrap()
                .contains("8.8.8.8".parse().unwrap())
        );
        for bad in [
            "10.0.0.0/33",
            "10.0.0.0/255",
            "::1/255",
            "not-an-ip",
            "10.0.0.0/x",
            "",
        ] {
            assert!(Cidr::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn prefix_files_come_in_the_feed_shape_or_as_lines() {
        let feed = r#"{"creationTime":"2026-09-18T00:18:04","prefixes":[{"ipv4Prefix":"104.192.219.204/30"},{"ipv6Prefix":"2a09:bac0::/32"},{"other":"x"}]}"#;
        let parsed = parse_prefix_file(feed).unwrap();
        assert_eq!(parsed.len(), 2);
        let lines = "# vendor\n10.1.0.0/16   # office\n\n192.0.2.7\n";
        assert_eq!(parse_prefix_file(lines).unwrap().len(), 2);
        assert!(parse_prefix_file("{\"prefixes\": 5}").is_err());
    }

    #[test]
    fn automatic_feed_rejects_partial_ambiguous_empty_and_oversized_lists() {
        let valid =
            r#"{"prefixes":[{"ipv4Prefix":"192.0.2.0/24"},{"ipv6Prefix":"2001:db8::/32"}]}"#;
        assert_eq!(parse_openai_feed(valid).unwrap().len(), 2);
        for bad in [
            "not JSON",
            "[]",
            r#"{"prefixes":[]}"#,
            r#"{"prefixes":[{}]}"#,
            r#"{"prefixes":[{"ipv4Prefix":"192.0.2.0/24"},{"other":"unknown"}]}"#,
            r#"{"prefixes":[{"ipv4Prefix":"0.0.0.0/0"}]}"#,
            r#"{"prefixes":[{"ipv6Prefix":"::/0"}]}"#,
            r#"{"prefixes":[{"ipv4Prefix":"192.0.2.0/255"}]}"#,
            r#"{"prefixes":[{"ipv4Prefix":"192.0.2.1"}]}"#,
            r#"{"prefixes":[{"ipv4Prefix":"2001:db8::/32"}]}"#,
            r#"{"prefixes":[{"ipv6Prefix":"192.0.2.0/24"}]}"#,
            r#"{"prefixes":[{"ipv4Prefix":true}]}"#,
            r#"{"prefixes":[{"ipv4Prefix":"192.0.2.0/24","ipv6Prefix":"2001:db8::/32"}]}"#,
        ] {
            assert!(parse_openai_feed(bad).is_err(), "{bad}");
        }
        assert!(parse_openai_feed(&" ".repeat(MAX_FEED_BYTES + 1)).is_err());
        let entry = serde_json::json!({"ipv4Prefix":"192.0.2.0/24"});
        let maximum = serde_json::json!({"prefixes": vec![entry.clone(); MAX_FEED_PREFIXES]});
        assert_eq!(
            parse_openai_feed(&maximum.to_string()).unwrap().len(),
            MAX_FEED_PREFIXES
        );
        let too_many = serde_json::json!({"prefixes": vec![entry; MAX_FEED_PREFIXES + 1]});
        assert!(parse_openai_feed(&too_many.to_string()).is_err());
    }

    fn feed_for(address: &str) -> Result<Vec<Cidr>> {
        parse_openai_feed(&serde_json::json!({"prefixes":[{"ipv4Prefix":address}]}).to_string())
    }

    #[tokio::test]
    async fn refresh_replaces_the_snapshot_and_failures_preserve_last_good() {
        let list = Allowlist::parse(&["openai".into(), "anthropic".into()], "x-forwarded-for")
            .unwrap()
            .unwrap();
        assert!(list.auto_refresh());
        assert_eq!(list.len(), 1, "no automatic admission before initial fetch");
        assert!(!list.allows(&request_from("x-forwarded-for", "192.0.2.1")));
        list.refresh_with(|| feed_for("192.0.2.0/24"))
            .await
            .unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.allows(&request_from("x-forwarded-for", "192.0.2.1")));
        assert!(list.allows(&request_from("x-forwarded-for", "160.79.104.1")));
        assert!(
            list.refresh_with(|| bail!("network unavailable"))
                .await
                .is_err()
        );
        assert!(
            list.refresh_with(|| parse_openai_feed(r#"{"prefixes":[]}"#))
                .await
                .is_err()
        );
        assert!(list.allows(&request_from("x-forwarded-for", "192.0.2.1")));
        assert!(!list.allows(&request_from("x-forwarded-for", "203.0.113.1")));
        list.refresh_with(|| feed_for("203.0.113.0/24"))
            .await
            .unwrap();
        assert!(!list.allows(&request_from("x-forwarded-for", "192.0.2.1")));
        assert!(list.allows(&request_from("x-forwarded-for", "203.0.113.1")));
        assert!(list.allows(&request_from("x-forwarded-for", "160.79.104.1")));
    }

    #[tokio::test]
    async fn explicit_sources_never_fetch_and_initial_failure_does_not_allow_all() {
        let fixed = Allowlist::parse(&["192.0.2.0/24".into()], "x-forwarded-for")
            .unwrap()
            .unwrap();
        assert!(!fixed.auto_refresh());
        fixed
            .refresh_with(|| panic!("explicit CIDRs must not fetch"))
            .await
            .unwrap();
        let automatic = Allowlist::parse(&["openai".into(), "openai".into()], "x-forwarded-for")
            .unwrap()
            .unwrap();
        assert!(automatic.refresh_with(|| bail!("offline")).await.is_err());
        assert_eq!(automatic.len(), 0);
        assert!(!automatic.allows(&request_from("x-forwarded-for", "192.0.2.1")));
        automatic
            .refresh_with(|| feed_for("192.0.2.0/24"))
            .await
            .unwrap();
        assert_eq!(
            automatic.len(),
            1,
            "duplicate preset does not duplicate refreshes"
        );
    }

    #[tokio::test]
    async fn in_flight_fetch_does_not_block_admission_or_apply_after_cancellation() {
        let list = std::sync::Arc::new(
            Allowlist::parse(&["openai".into()], "x-forwarded-for")
                .unwrap()
                .unwrap(),
        );
        list.refresh_with(|| feed_for("192.0.2.0/24"))
            .await
            .unwrap();
        let (started, start) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let (finished, finish) = tokio::sync::oneshot::channel();
        let updating = list.clone();
        let job = tokio::spawn(async move {
            updating
                .refresh_with(move || {
                    started.send(()).unwrap();
                    released.blocking_recv().unwrap();
                    let result = feed_for("203.0.113.0/24");
                    finished.send(()).unwrap();
                    result
                })
                .await
        });
        start.await.unwrap();
        assert!(list.allows(&request_from("x-forwarded-for", "192.0.2.1")));
        assert!(!list.allows(&request_from("x-forwarded-for", "203.0.113.1")));
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        finish.await.unwrap();
        assert!(list.allows(&request_from("x-forwarded-for", "192.0.2.1")));
        assert!(!list.allows(&request_from("x-forwarded-for", "203.0.113.1")));
    }

    #[test]
    #[ignore = "live vendor endpoint; run explicitly for acceptance"]
    fn live_openai_feed_is_accepted() {
        let prefixes = fetch_openai_feed().unwrap();
        assert!(!prefixes.is_empty() && prefixes.len() <= MAX_FEED_PREFIXES);
    }

    #[test]
    fn the_allowlist_checks_the_tunnels_header_and_reloads_a_changed_file() {
        assert!(Allowlist::parse(&[], "cf-connecting-ip").unwrap().is_none());
        assert!(
            Allowlist::parse(&["anthropic".into()], "").is_err(),
            "a header is required"
        );
        let dir = tempfile::tempdir().unwrap();
        let feed = dir.path().join("openai.json");
        std::fs::write(
            &feed,
            r#"{"prefixes":[{"ipv4Prefix":"104.192.219.204/30"}]}"#,
        )
        .unwrap();
        let list = Allowlist::parse(
            &[
                "anthropic".into(),
                format!("@{}", feed.display()),
                "192.0.2.0/24".into(),
            ],
            "CF-Connecting-IP",
        )
        .unwrap()
        .unwrap();
        assert_eq!(list.len(), 3);
        assert!(list.allows(&request_from("cf-connecting-ip", "160.79.104.20")));
        assert!(list.allows(&request_from("cf-connecting-ip", "104.192.219.206")));
        assert!(list.allows(&request_from("cf-connecting-ip", "192.0.2.9")));
        assert!(!list.allows(&request_from("cf-connecting-ip", "8.8.8.8")));
        assert!(
            !list.allows(&request_from("x-forwarded-for", "160.79.104.20")),
            "wrong header"
        );
        assert!(!list.allows(&request_from("cf-connecting-ip", "not-an-ip")));
        assert!(!list.allows(&Request {
            method: "POST".into(),
            target: "/mcp".into(),
            headers: vec![],
            body: vec![],
        }));
        // The feed is refreshed on disk: the new prefix counts, the old one does not.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&feed, r#"{"prefixes":[{"ipv4Prefix":"203.0.113.0/24"}]}"#).unwrap();
        let later = std::time::SystemTime::now();
        filetime::set_file_mtime(&feed, filetime::FileTime::from_system_time(later)).unwrap();
        assert!(list.allows(&request_from("cf-connecting-ip", "203.0.113.4")));
        assert!(!list.allows(&request_from("cf-connecting-ip", "104.192.219.206")));
        let forwarded = Allowlist::parse(&["192.0.2.0/24".into()], "x-forwarded-for")
            .unwrap()
            .unwrap();
        assert!(forwarded.allows(&request_from("x-forwarded-for", "192.0.2.5, 10.0.0.1")));
        assert!(!forwarded.allows(&request_from("x-forwarded-for", "10.0.0.1, 192.0.2.5")));
    }
}
