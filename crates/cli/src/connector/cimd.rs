//! Fetching a Client ID Metadata Document: a request to a vendor's host for the document a
//! URL-formatted `client_id` names. Bounded in time and size, HTTPS
//! only, no redirects — the URL is the identity, so a document served
//! from anywhere else is not the one asked for. What the document may
//! say is `oauth::Store::admit_metadata_client`'s business.

use std::time::Duration;

use serde_json::Value;

use super::oauth::{MAX_METADATA_BYTES, OAuthError};

/// The whole fetch, connect to last byte.
pub const FETCH_DEADLINE: Duration = Duration::from_secs(10);

/// What turns a `client_id` URL into its document: the HTTPS fetch in
/// production, a table in tests.
pub type Fetcher = dyn Fn(&str) -> Result<Value, OAuthError> + Send + Sync;

/// The client a document is fetched with: [`FETCH_DEADLINE`], no
/// redirects, HTTPS only, every status an answer to read, and TLS verified
/// as every outbound client's is, against the roots the system trusts
/// ([`agentdocker_host::http`]).
fn client() -> ureq::Agent {
    agentdocker_host::http::Policy {
        deadline: FETCH_DEADLINE,
        max_redirects: 0,
        https_only: true,
        status_as_error: false,
        user_agent: Some(concat!("agentdocker/", env!("CARGO_PKG_VERSION"))),
    }
    .agent()
}

pub fn fetch(url: &str) -> Result<Value, OAuthError> {
    let agent = client();
    let not_fetched = |why: String| {
        OAuthError::new(
            "invalid_client",
            format!("the client metadata at {url} could not be fetched: {why}"),
        )
    };
    let mut response = agent
        .get(url)
        .header("Accept", "application/json")
        .call()
        .map_err(|error| not_fetched(error.to_string()))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(not_fetched(format!("HTTP {status}")));
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(MAX_METADATA_BYTES as u64)
        .read_to_string()
        .map_err(|error| not_fetched(error.to_string()))?;
    serde_json::from_str(&text).map_err(|error| {
        OAuthError::new(
            "invalid_client_metadata",
            format!("the client metadata at {url} is not JSON: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing but HTTPS is ever fetched, and a refusal says so without
    /// a request leaving: a plain-http URL fails before any connection.
    #[test]
    fn only_https_is_fetched() {
        let error = fetch("http://127.0.0.1:9/client.json").unwrap_err();
        assert_eq!(error.code, "invalid_client");
        assert!(
            error.description.contains("could not be fetched"),
            "{}",
            error.description
        );
    }

    /// A document is fetched with the platform's own verifier and trust
    /// roots, not the bundled Mozilla set ureq would use unasked, with
    /// verification on; the fetch keeps its deadline, takes no redirect,
    /// nothing but HTTPS, and reads every status itself. A host that hangs
    /// up mid-handshake is a refusal, not a crash: the verifier is built
    /// before the first message.
    #[test]
    fn a_document_is_fetched_trusting_the_platforms_roots() {
        use ureq::config::AutoHeaderValue;
        use ureq::tls::RootCerts;
        let agent = client();
        let config = agent.config();
        let tls = config.tls_config();
        assert!(
            matches!(tls.root_certs(), RootCerts::PlatformVerifier),
            "roots: {:?}",
            tls.root_certs()
        );
        assert!(!tls.disable_verification());
        assert_eq!(config.timeouts().global, Some(FETCH_DEADLINE));
        assert_eq!(config.max_redirects(), 0);
        assert!(config.https_only());
        assert!(!config.http_status_as_error());
        assert!(matches!(
            config.user_agent(),
            AutoHeaderValue::Provided(ua) if ua.starts_with("agentdocker/")
        ));

        let hangs_up = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = hangs_up.local_addr().unwrap().port();
        let host = std::thread::spawn(move || drop(hangs_up.accept().unwrap()));
        let error = fetch(&format!("https://127.0.0.1:{port}/client.json")).unwrap_err();
        host.join().unwrap();
        assert_eq!(error.code, "invalid_client");
        assert!(
            error.description.contains("could not be fetched"),
            "{}",
            error.description
        );
    }
}
