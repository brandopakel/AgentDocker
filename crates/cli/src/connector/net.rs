//! Who may reach the vendor-facing endpoints, by the client address the
//! tunnel reports. Behind a tunnel every TCP peer is the tunnel itself, so
//! the address worth checking is the one the tunnel writes into a header
//! (`cf-connecting-ip` for cloudflared). The vendors publish their egress
//! ranges: Anthropic's is one block; OpenAI's is a JSON feed of a few
//! hundred prefixes that changes, so it is read from a file the person
//! keeps fresh, never fetched from here.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};

use super::http::Request;

/// Anthropic's published outbound range for connectors, from its
/// connector documentation. Stable enough to name; still one line to
/// change.
pub const ANTHROPIC_EGRESS: &[&str] = &["160.79.104.0/21"];

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
                prefix
                    .parse::<u8>()
                    .with_context(|| format!("{text}: the prefix length is not a number"))?,
            ),
            None => (text, u8::MAX),
        };
        let address: IpAddr = address
            .parse()
            .with_context(|| format!("{text}: not an IP address"))?;
        let bits = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix = if prefix == u8::MAX { bits } else { prefix };
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

/// The allowlist as configured: literal prefixes, the Anthropic preset,
/// and files re-read when they change, so a refreshed feed applies
/// without a restart.
pub struct Allowlist {
    header: String,
    fixed: Vec<Cidr>,
    files: Mutex<Vec<FromFile>>,
}

impl Allowlist {
    /// `values` are CIDRs, `anthropic`, or `@<path>`. `header` names the
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
        for value in values {
            if value == "anthropic" {
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
        for bad in ["10.0.0.0/33", "not-an-ip", "10.0.0.0/x", ""] {
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
