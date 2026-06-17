use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

use url::Url;

static SERVICES_MODE: AtomicU8 = AtomicU8::new(ServicesMode::Online as u8);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServicesMode {
    #[default]
    Online = 0,
    LocalOnly = 1,
}

impl ServicesMode {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::LocalOnly,
            _ => Self::Online,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("NetworkPolicyDenied: {purpose} to {url} is disabled in local-only services mode")]
pub struct NetworkPolicyDenied {
    purpose: String,
    url: String,
}

impl NetworkPolicyDenied {
    pub fn purpose(&self) -> &str {
        &self.purpose
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

pub fn set_services_mode(mode: ServicesMode) {
    SERVICES_MODE.store(mode as u8, Ordering::SeqCst);
}

pub fn services_mode() -> ServicesMode {
    ServicesMode::from_u8(SERVICES_MODE.load(Ordering::SeqCst))
}

pub fn is_local_only() -> bool {
    services_mode() == ServicesMode::LocalOnly
}

pub fn check_url_str(url: &str, purpose: &str) -> Result<(), NetworkPolicyDenied> {
    match Url::parse(url) {
        Ok(url) => check_url(&url, purpose),
        Err(_) if is_local_only() => Err(NetworkPolicyDenied {
            purpose: purpose.to_owned(),
            url: url.to_owned(),
        }),
        Err(_) => Ok(()),
    }
}

pub fn check_url(url: &Url, purpose: &str) -> Result<(), NetworkPolicyDenied> {
    check_url_for_mode(services_mode(), url, purpose)
}

pub fn check_url_for_mode(
    mode: ServicesMode,
    url: &Url,
    purpose: &str,
) -> Result<(), NetworkPolicyDenied> {
    if mode == ServicesMode::Online || !is_guarded_scheme(url.scheme()) || is_loopback_url(url) {
        return Ok(());
    }

    Err(NetworkPolicyDenied {
        purpose: purpose.to_owned(),
        url: url.as_str().to_owned(),
    })
}

fn is_guarded_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss")
}

fn is_loopback_url(url: &Url) -> bool {
    // Use the url crate's Host enum rather than manual string parsing
    // of the host. The url crate already parses IPv4 and IPv6 into
    // standard library types with proper is_loopback() semantics,
    // including the bracketed IPv6 form (e.g. [::1]) which the
    // previous string-strip code happened to handle but only by
    // accident.
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback()
                || ip.to_ipv4_mapped().map_or(false, |ipv4| ipv4.is_loopback())
        }
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_only_allows_loopback_http_urls() {
        for url in [
            "http://localhost:11434/v1/chat/completions",
            "http://127.0.0.1:8080",
            "http://127.7.8.9:8080",
            "http://[::1]:8080",
            "ws://localhost:8080/graphql",
        ] {
            let url = Url::parse(url).unwrap();
            assert_eq!(
                check_url_for_mode(ServicesMode::LocalOnly, &url, "test"),
                Ok(())
            );
        }
    }

    #[test]
    fn local_only_denies_public_http_and_websocket_urls() {
        for url in [
            "https://app.warp.dev",
            "wss://rtc.app.warp.dev/graphql/v2",
            "https://192.168.1.1",
        ] {
            let url = Url::parse(url).unwrap();
            assert!(check_url_for_mode(ServicesMode::LocalOnly, &url, "test").is_err());
        }
    }

    #[test]
    fn local_only_does_not_guard_file_like_urls() {
        for url in ["file:///tmp/font.ttf", "data:text/plain,hello"] {
            let url = Url::parse(url).unwrap();
            assert_eq!(
                check_url_for_mode(ServicesMode::LocalOnly, &url, "test"),
                Ok(())
            );
        }
    }

    #[test]
    fn online_mode_allows_public_urls() {
        let url = Url::parse("https://app.warp.dev").unwrap();
        assert_eq!(
            check_url_for_mode(ServicesMode::Online, &url, "test"),
            Ok(())
        );
    }

    #[test]
    fn ipv4_mapped_ipv6_loopback_is_recognized() {
        // IPv4-mapped IPv6 addresses like ::ffff:127.0.0.1 should be treated as loopback
        let url = Url::parse("http://[::ffff:127.0.0.1]:8080").unwrap();
        assert!(is_loopback_url(&url));
    }

    #[test]
    fn ipv6_link_local_is_not_loopback() {
        // Link-local addresses (fe80::/10) are NOT loopback
        let url = Url::parse("http://[fe80::1]:8080").unwrap();
        assert!(!is_loopback_url(&url));
    }

}
