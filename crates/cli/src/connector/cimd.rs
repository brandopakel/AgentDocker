//! Fetching a Client ID Metadata Document: the one outbound request this
//! connector ever makes, to a vendor's host, for the document a
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

pub fn fetch(url: &str) -> Result<Value, OAuthError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_DEADLINE))
        .max_redirects(0)
        .https_only(true)
        .http_status_as_error(false)
        .user_agent(concat!("agentdocker/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
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
}
