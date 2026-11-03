//! Configured AI endpoints: hosts the user typed into an AI endpoint setting.
//!
//! Local-only mode allows loopback, plus the private hosts the user listed (see
//! [`crate::private_hosts`]). A user who enters `https://api.example.com/v1` as the base URL of an
//! AI endpoint has already said where requests should go, so asking them to repeat the host in an
//! allow-list would be redundant. The origin of every configured endpoint is therefore allowed,
//! and nothing else is.
//!
//! Rules:
//! - Only `https` endpoints are allowed. A plain `http` base URL on a non-loopback host is not
//!   added. Loopback `http` is already allowed by the base policy, and a private-network `http`
//!   host still has to be listed under the allowed private hosts.
//! - Matching is by origin: scheme, host and port. Paths are not compared, so `/v1/models` and
//!   `/v1/chat/completions` on a configured host are both allowed. A different port or a
//!   subdomain is a different origin and stays denied.
//! - Host names are matched, unlike for private hosts. With `https`, the TLS certificate binds the
//!   name to the server, so pointing the name at another address (DNS rebinding) fails the
//!   handshake instead of reaching a different machine.
//! - Redirects are re-checked against the same list by [`crate::redirect_policy`], so a configured
//!   host cannot bounce a request to a host that was not configured.
//!
//! Several settings models own endpoint URLs, so the list is kept per [`EndpointSource`] and
//! each source replaces only its own entries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::RwLock;

use url::Url;

/// Environment variable carrying the configured endpoints to child processes, comma separated, in
/// the form [`ConfiguredEndpoint`] displays and parses (`https://host:port`).
pub const ENV_ALLOWED_ENDPOINTS: &str = "WARP_ALLOWED_ENDPOINTS";

/// The only scheme a configured endpoint may use.
const ENDPOINT_SCHEME: &str = "https";

/// Who published a set of configured endpoints. Each source replaces only its own entries.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EndpointSource {
    /// AI settings: the Local AI base URL and the custom inference endpoint definitions.
    AiSettings,
    /// Per-profile inference settings: the profile base URL and its custom endpoints.
    ProfileSettings,
    /// The list a parent process handed down through [`ENV_ALLOWED_ENDPOINTS`].
    Parent,
}

/// The origin of a configured `https` endpoint.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConfiguredEndpoint {
    /// Lowercase host as `url` serializes it: a domain (punycode for IDNs) or an IP literal,
    /// IPv6 in brackets.
    host: String,
    port: u16,
}

impl ConfiguredEndpoint {
    /// The endpoint origin of a base URL the user configured, or `None` if the URL does not
    /// qualify: it does not parse, is not `https`, or has no host.
    pub fn from_base_url(base_url: &str) -> Option<Self> {
        let url = Url::parse(base_url.trim()).ok()?;
        Self::from_url(&url)
    }

    fn from_url(url: &Url) -> Option<Self> {
        if url.scheme() != ENDPOINT_SCHEME {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let port = url.port_or_known_default()?;
        Some(Self { host, port })
    }

    /// Parses the `https://host:port` form produced by `Display`, as a child process receives it.
    pub fn parse(entry: &str) -> Result<Self, EndpointsError> {
        Self::from_base_url(entry).ok_or_else(|| EndpointsError::Invalid(entry.to_owned()))
    }
}

impl fmt::Display for ConfiguredEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{ENDPOINT_SCHEME}://{}:{}", self.host, self.port)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EndpointsError {
    #[error("invalid configured endpoint {0:?}: expected https://host:port")]
    Invalid(String),
}

static CONFIGURED_ENDPOINTS: RwLock<BTreeMap<EndpointSource, BTreeSet<ConfiguredEndpoint>>> =
    RwLock::new(BTreeMap::new());

/// Replaces the endpoints published by `source`. Base URLs that do not qualify (see
/// [`ConfiguredEndpoint::from_base_url`]) are skipped.
pub fn set_configured_endpoints<'a>(
    source: EndpointSource,
    base_urls: impl IntoIterator<Item = &'a str>,
) {
    let endpoints = base_urls
        .into_iter()
        .filter_map(ConfiguredEndpoint::from_base_url)
        .collect();
    set_endpoints(source, endpoints);
}

fn set_endpoints(source: EndpointSource, endpoints: BTreeSet<ConfiguredEndpoint>) {
    let mut all = CONFIGURED_ENDPOINTS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if endpoints.is_empty() {
        all.remove(&source);
    } else {
        all.insert(source, endpoints);
    }
}

/// Every configured endpoint, from all sources.
pub fn configured_endpoints() -> BTreeSet<ConfiguredEndpoint> {
    CONFIGURED_ENDPOINTS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .flatten()
        .cloned()
        .collect()
}

/// Whether `url` targets the origin of a configured endpoint.
pub(crate) fn is_configured_endpoint_url(url: &Url) -> bool {
    let Some(endpoint) = ConfiguredEndpoint::from_url(url) else {
        return false;
    };
    CONFIGURED_ENDPOINTS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .any(|endpoints| endpoints.contains(&endpoint))
}

/// Formats endpoints for [`ENV_ALLOWED_ENDPOINTS`].
pub(crate) fn format_endpoints<'a>(
    endpoints: impl IntoIterator<Item = &'a ConfiguredEndpoint>,
) -> String {
    endpoints
        .into_iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Parses a list in the form [`format_endpoints`] writes. An invalid entry fails the whole list.
pub fn parse_endpoints(input: &str) -> Result<BTreeSet<ConfiguredEndpoint>, EndpointsError> {
    input
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ConfiguredEndpoint::parse)
        .collect()
}

/// Applies the list a parent handed down through [`ENV_ALLOWED_ENDPOINTS`].
pub(crate) fn set_parent_endpoints(endpoints: BTreeSet<ConfiguredEndpoint>) {
    set_endpoints(EndpointSource::Parent, endpoints);
}

#[cfg(test)]
#[path = "endpoints_tests.rs"]
mod tests;
