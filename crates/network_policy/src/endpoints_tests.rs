use std::sync::{Mutex, MutexGuard};

use url::Url;

use super::*;

/// The endpoint list is process-wide, so tests that change it run one at a time and start empty.
static ENDPOINTS_LOCK: Mutex<()> = Mutex::new(());

struct EmptyEndpoints(#[allow(dead_code)] MutexGuard<'static, ()>);

impl EmptyEndpoints {
    fn new() -> Self {
        let guard = ENDPOINTS_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clear();
        Self(guard)
    }
}

impl Drop for EmptyEndpoints {
    fn drop(&mut self) {
        clear();
    }
}

fn clear() {
    CONFIGURED_ENDPOINTS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

fn allowed(url: &str) -> bool {
    is_configured_endpoint_url(&Url::parse(url).unwrap())
}

#[test]
fn https_base_url_allows_its_origin_on_any_path() {
    let _guard = EmptyEndpoints::new();
    set_configured_endpoints(EndpointSource::AiSettings, ["https://api.minimax.io/v1"]);

    assert!(allowed("https://api.minimax.io/v1/chat/completions"));
    assert!(allowed("https://api.minimax.io/v1/models"));
    assert!(allowed("https://API.MiniMax.io:443/other"));
}

#[test]
fn other_hosts_ports_and_schemes_stay_denied() {
    let _guard = EmptyEndpoints::new();
    set_configured_endpoints(EndpointSource::AiSettings, ["https://api.minimax.io/v1"]);

    assert!(!allowed("http://api.minimax.io/v1/chat/completions"));
    assert!(!allowed("https://api.minimax.io:8443/v1"));
    assert!(!allowed("https://evil.api.minimax.io/v1"));
    assert!(!allowed("https://api.openai.com/v1"));
    assert!(!allowed("wss://api.minimax.io/v1"));
}

#[test]
fn plain_http_and_unparsable_base_urls_add_nothing() {
    let _guard = EmptyEndpoints::new();
    set_configured_endpoints(
        EndpointSource::AiSettings,
        ["http://api.minimax.io/v1", "not a url", "", "https://"],
    );

    assert!(configured_endpoints().is_empty());
    assert!(!allowed("http://api.minimax.io/v1"));
    assert!(!allowed("https://api.minimax.io/v1"));
}

#[test]
fn each_source_replaces_only_its_own_entries() {
    let _guard = EmptyEndpoints::new();
    set_configured_endpoints(EndpointSource::AiSettings, ["https://a.example/v1"]);
    set_configured_endpoints(EndpointSource::ProfileSettings, ["https://b.example/v1"]);
    assert!(allowed("https://a.example/x") && allowed("https://b.example/x"));

    // Clearing the URL in one setting revokes only that host.
    set_configured_endpoints(EndpointSource::AiSettings, std::iter::empty());
    assert!(!allowed("https://a.example/x"));
    assert!(allowed("https://b.example/x"));
}

#[test]
fn display_round_trips_through_parse() {
    let endpoint = ConfiguredEndpoint::from_base_url("https://Api.Example.com/v1/").unwrap();
    assert_eq!(endpoint.to_string(), "https://api.example.com:443");
    assert_eq!(
        ConfiguredEndpoint::parse(&endpoint.to_string()),
        Ok(endpoint)
    );

    let ipv6 = ConfiguredEndpoint::from_base_url("https://[2001:db8::1]:8443/v1").unwrap();
    assert_eq!(ipv6.to_string(), "https://[2001:db8::1]:8443");
    assert_eq!(ConfiguredEndpoint::parse(&ipv6.to_string()), Ok(ipv6));
}

#[test]
fn parse_endpoints_rejects_the_whole_list_on_one_bad_entry() {
    assert_eq!(
        parse_endpoints("https://a.example:443, https://b.example:8443")
            .unwrap()
            .len(),
        2
    );
    assert!(parse_endpoints("").unwrap().is_empty());
    assert_eq!(
        parse_endpoints("https://a.example:443,http://b.example:80"),
        Err(EndpointsError::Invalid)
    );
}

#[test]
fn local_only_policy_admits_a_configured_https_endpoint_and_nothing_else() {
    let _guard = EmptyEndpoints::new();
    let check = |url: &str| {
        crate::check_url_for_mode(
            crate::ServicesMode::LocalOnly,
            &Url::parse(url).unwrap(),
            "test",
        )
    };
    assert!(check("https://api.minimax.io/v1/chat/completions").is_err());

    set_configured_endpoints(EndpointSource::AiSettings, ["https://api.minimax.io/v1"]);
    assert!(check("https://api.minimax.io/v1/chat/completions").is_ok());
    assert!(check("http://api.minimax.io/v1/chat/completions").is_err());
    assert!(check("https://api.openai.com/v1/chat/completions").is_err());
}
