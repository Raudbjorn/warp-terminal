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
    check_url_str_for_mode(services_mode(), url, purpose)
}

/// Like [`check_url_for_mode`], but for unparsed URLs. In local-only mode a URL that fails to
/// parse is denied: it can't be classified as loopback, and some are not actually local. For
/// example, the protocol-relative `//example.com/ws` fails to parse without a base, but a browser
/// resolves it against the page's scheme and connects to `example.com`.
pub fn check_url_str_for_mode(
    mode: ServicesMode,
    url: &str,
    purpose: &str,
) -> Result<(), NetworkPolicyDenied> {
    match Url::parse(url) {
        Ok(url) => check_url_for_mode(mode, &url, purpose),
        Err(_) if mode == ServicesMode::LocalOnly => Err(NetworkPolicyDenied {
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

/// Maximum number of redirects followed by [`redirect_policy`]. Matches reqwest's default limit.
#[cfg(not(target_family = "wasm"))]
const MAX_REDIRECTS: usize = 10;

/// A reqwest redirect policy that re-validates every redirect target against the network policy.
///
/// [`check_url`] only sees the URL a request starts with. reqwest's default policy then follows up
/// to 10 redirects without consulting it, so in local-only mode a loopback URL could redirect to a
/// public host and leave the machine. Any client that may reach a non-allowlisted host must use
/// this policy. A denied target fails the request with [`NetworkPolicyDenied`] as the error
/// source, rather than handing the 3xx response back to the caller as if it were the result.
///
/// Unlike [`reqwest::redirect::Policy::custom`] on its own, this keeps reqwest's default hop limit.
#[cfg(not(target_family = "wasm"))]
pub fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        // `previous` includes the initial URL, which is not a redirect. This mirrors the check in
        // `Policy::limited`.
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        match check_url(attempt.url(), "redirect target") {
            Ok(()) => attempt.follow(),
            Err(err) => attempt.error(err),
        }
    })
}

fn is_guarded_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss")
}

fn is_loopback_url(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
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
    fn local_only_denies_unparseable_urls() {
        // Relative and protocol-relative URLs must not be waved through: a browser resolves
        // `//example.com/ws` to a public host.
        for url in ["/ws", "//example.com/ws", "example.com/ws", ""] {
            assert!(
                check_url_str_for_mode(ServicesMode::LocalOnly, url, "test").is_err(),
                "{url:?} should be denied"
            );
            assert_eq!(
                check_url_str_for_mode(ServicesMode::Online, url, "test"),
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
}
