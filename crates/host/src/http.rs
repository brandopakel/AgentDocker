//! The one way an outbound HTTP client is built: the daemon's webhook
//! sinks, Supermemory mirror and screen judge, and the connector's Client
//! ID Metadata Document fetch and egress feed. Each client says its own
//! deadline, redirects, schemes and how a status is taken; how its peer's
//! certificate is verified is not among them. That is always the
//! platform's own verifier against the roots the system trusts
//! (`rustls-platform-verifier`: the keychain's trust settings on macOS,
//! the certificate store on Windows, the system's CA bundle on Linux), so
//! an authority the person or their organisation added is trusted and the
//! Mozilla set ureq bundles is never used.
//!
//! A body's size is bounded where it is read
//! (`Body::with_config().limit`), since ureq bounds a body per read rather
//! than per client.

use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig};

/// What one client may do, every field the caller's to choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The whole request, from connecting to the answer's last byte.
    pub deadline: Duration,
    /// How many redirects are followed; 0 follows none.
    pub max_redirects: u32,
    /// Refuse a URL that is not `https://`, before any connection.
    pub https_only: bool,
    /// Whether a 4xx or 5xx is an error rather than an answer to read.
    pub status_as_error: bool,
    /// The `User-Agent` sent; `None` sends ureq's own.
    pub user_agent: Option<&'static str>,
}

impl Policy {
    /// A client keeping to this policy, verifying TLS as every outbound
    /// client does.
    pub fn agent(&self) -> ureq::Agent {
        let builder = ureq::Agent::config_builder()
            .timeout_global(Some(self.deadline))
            .max_redirects(self.max_redirects)
            .https_only(self.https_only)
            .http_status_as_error(self.status_as_error)
            .tls_config(tls());
        match self.user_agent {
            Some(user_agent) => builder.user_agent(user_agent),
            None => builder,
        }
        .build()
        .into()
    }
}

/// Verification on, by the platform's verifier and the system's roots.
fn tls() -> TlsConfig {
    TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use ureq::config::AutoHeaderValue;

    const POLICY: Policy = Policy {
        deadline: Duration::from_secs(5),
        max_redirects: 0,
        https_only: true,
        status_as_error: false,
        user_agent: None,
    };

    /// A client keeps to the policy it was given, field by field, and
    /// verifies with the platform's verifier and trust roots — never the
    /// bundled Mozilla set ureq would use unasked — with verification on.
    #[test]
    fn a_client_keeps_its_policy_and_trusts_the_platforms_roots() {
        let agent = POLICY.agent();
        let config = agent.config();
        let tls = config.tls_config();
        assert!(
            matches!(tls.root_certs(), RootCerts::PlatformVerifier),
            "roots: {:?}",
            tls.root_certs()
        );
        assert!(!tls.disable_verification());
        assert_eq!(config.timeouts().global, Some(POLICY.deadline));
        assert_eq!(config.max_redirects(), 0);
        assert!(config.https_only());
        assert!(!config.http_status_as_error());
        assert!(matches!(config.user_agent(), AutoHeaderValue::Default));

        let other = Policy {
            deadline: Duration::from_secs(30),
            max_redirects: 3,
            https_only: false,
            status_as_error: true,
            user_agent: Some("agentdocker/test"),
        }
        .agent();
        let config = other.config();
        assert!(matches!(
            config.tls_config().root_certs(),
            RootCerts::PlatformVerifier
        ));
        assert_eq!(config.timeouts().global, Some(Duration::from_secs(30)));
        assert_eq!(config.max_redirects(), 3);
        assert!(!config.https_only());
        assert!(config.http_status_as_error());
        assert!(
            matches!(config.user_agent(), AutoHeaderValue::Provided(ua) if ua.as_str() == "agentdocker/test")
        );
    }

    /// An https request builds the platform verifier before its first
    /// message, and ureq panics there when the feature providing it is
    /// missing: a peer that hangs up mid-handshake is a transport error,
    /// not a crash.
    #[test]
    fn a_handshake_builds_the_platform_verifier() {
        let hangs_up = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = hangs_up.local_addr().unwrap().port();
        let peer = std::thread::spawn(move || drop(hangs_up.accept().unwrap()));
        let error = POLICY
            .agent()
            .get(format!("https://127.0.0.1:{port}/"))
            .call()
            .unwrap_err();
        peer.join().unwrap();
        assert!(
            matches!(
                error,
                ureq::Error::Io(_) | ureq::Error::Tls(_) | ureq::Error::Rustls(_)
            ),
            "{error:?}"
        );
    }
}
