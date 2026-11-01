use std::net::IpAddr;
use std::sync::Mutex;

use super::*;
use crate::{ServicesMode, check_url_for_mode};

// The allowed list is process-wide, so tests that change it take this lock. They use ranges that
// the other tests in this crate do not (they deny 192.168.1.1), so they cannot affect each other.
static ALLOWED_LIST_LOCK: Mutex<()> = Mutex::new(());

/// Sets the allowed list for the duration of a test and clears it afterwards.
struct AllowedList(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl AllowedList {
    fn set(input: &str) -> Self {
        let guard = ALLOWED_LIST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set_allowed_private_hosts(parse_allowed_private_hosts(input).unwrap());
        Self(guard)
    }
}

impl Drop for AllowedList {
    fn drop(&mut self) {
        set_allowed_private_hosts(Vec::new());
    }
}

fn local_only(url: &str) -> bool {
    let url = Url::parse(url).unwrap();
    check_url_for_mode(ServicesMode::LocalOnly, &url, "test").is_ok()
}

#[test]
fn parses_addresses_and_ranges_inside_private_space() {
    for (entry, shown) in [
        ("10.0.0.5", "10.0.0.5"),
        ("192.168.1.0/24", "192.168.1.0/24"),
        ("100.64.0.0/10", "100.64.0.0/10"),
        ("172.31.255.255", "172.31.255.255"),
        ("169.254.1.1", "169.254.1.1"),
        ("fd00::/8", "fd00::/8"),
        ("[fd00::1]", "fd00::1"),
        ("fe80::/10", "fe80::/10"),
        // Host bits are cleared, so `10.1.2.3/8` means the whole /8.
        ("10.1.2.3/8", "10.0.0.0/8"),
        ("  10.0.0.5  ", "10.0.0.5"),
    ] {
        assert_eq!(
            PrivateNetwork::parse(entry).unwrap().to_string(),
            shown,
            "entry: {entry:?}"
        );
    }
}

#[test]
fn ipv4_mapped_entries_are_stored_as_ipv4() {
    for (entry, shown) in [
        ("::ffff:192.168.1.50", "192.168.1.50"),
        ("[::ffff:10.0.0.5]", "10.0.0.5"),
        // The prefix counts bits of the 128-bit address: /120 is the IPv4 /24.
        ("::ffff:192.168.1.0/120", "192.168.1.0/24"),
        ("::ffff:10.0.0.5/128", "10.0.0.5"),
        ("::ffff:10.1.2.3/104", "10.0.0.0/8"),
    ] {
        assert_eq!(
            PrivateNetwork::parse(entry).unwrap().to_string(),
            shown,
            "entry: {entry:?}"
        );
    }

    let mapped = PrivateNetwork::parse("::ffff:192.168.1.50").unwrap();
    assert_eq!(mapped, PrivateNetwork::parse("192.168.1.50").unwrap());
    assert!(mapped.contains("192.168.1.50".parse().unwrap()));
    assert!(mapped.contains("::ffff:192.168.1.50".parse().unwrap()));
    assert!(!mapped.contains("192.168.1.51".parse().unwrap()));

    // Mapped addresses outside private space stay rejected, and so does a prefix that reaches
    // past the mapped block into other IPv6 space.
    for entry in [
        "::ffff:8.8.8.8",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.0/95",
        "::ffff:0:0/0",
    ] {
        assert!(
            matches!(
                PrivateNetwork::parse(entry),
                Err(PrivateHostsError::NotPrivate(_))
            ),
            "entry: {entry:?}"
        );
    }
}

#[test]
fn rejects_entries_that_are_not_ip_literals_or_ranges() {
    for entry in [
        "",
        "example.com",
        "localhost",
        "10.0.0.256",
        "10.0.0.0/33",
        "10.0.0.0/x",
        "fd00::/129",
        "10.0.0.0/",
    ] {
        assert!(
            matches!(
                PrivateNetwork::parse(entry),
                Err(PrivateHostsError::Invalid(_))
            ),
            "entry: {entry:?}"
        );
    }
}

#[test]
fn rejects_entries_that_reach_outside_private_space() {
    for entry in [
        "8.8.8.8",
        "0.0.0.0/0",
        "::/0",
        // These straddle the edge of a private block.
        "10.0.0.0/7",
        "172.16.0.0/11",
        "192.168.0.0/15",
        "100.64.0.0/9",
        "fe00::/9",
        "fc00::/6",
        // Just outside 172.16.0.0/12 and 100.64.0.0/10.
        "172.32.0.1",
        "100.128.0.1",
        // Loopback is always allowed and needs no entry.
        "127.0.0.1",
        "::1",
    ] {
        assert!(
            matches!(
                PrivateNetwork::parse(entry),
                Err(PrivateHostsError::NotPrivate(_))
            ),
            "entry: {entry:?}"
        );
    }
}

#[test]
fn network_contains_only_its_own_addresses() {
    let network = PrivateNetwork::parse("192.168.1.0/24").unwrap();
    assert!(network.contains("192.168.1.200".parse().unwrap()));
    assert!(!network.contains("192.168.2.1".parse().unwrap()));
    // An IPv4-mapped IPv6 address is compared as IPv4.
    assert!(network.contains("::ffff:192.168.1.7".parse().unwrap()));
    assert!(!network.contains("fd00::1".parse::<IpAddr>().unwrap()));

    let v6 = PrivateNetwork::parse("fd12::/32").unwrap();
    assert!(v6.contains("fd12:0:1::9".parse().unwrap()));
    assert!(!v6.contains("fd13::1".parse().unwrap()));
    assert!(!v6.contains("10.0.0.1".parse().unwrap()));
}

#[test]
fn lists_accept_common_separators_and_fail_as_a_whole() {
    let list = parse_allowed_private_hosts("10.0.0.1, 10.0.0.2;10.0.0.3\n 100.64.0.0/10").unwrap();
    assert_eq!(list.len(), 4);
    assert_eq!(parse_allowed_private_hosts("").unwrap(), Vec::new());
    assert_eq!(parse_allowed_private_hosts(" , ;\n").unwrap(), Vec::new());
    // One bad entry rejects the list instead of silently dropping the rest.
    assert!(parse_allowed_private_hosts("10.0.0.1, example.com").is_err());
    assert!(parse_allowed_private_hosts("10.0.0.1, 8.8.8.8").is_err());
}

#[test]
fn local_only_allows_only_loopback_by_default() {
    let _list = AllowedList::set("");
    assert!(local_only("http://127.0.0.1:11434/v1"));
    assert!(local_only("http://localhost:11434/v1"));
    assert!(local_only("http://[::1]:11434/v1"));
    assert!(!local_only("http://10.77.1.2:11434/v1"));
    assert!(!local_only("http://100.100.0.7:11434/v1"));
    assert!(!local_only("https://example.com/v1"));
}

#[test]
fn local_only_allows_listed_private_hosts_and_nothing_else() {
    let _list = AllowedList::set("10.77.0.0/16, 100.100.0.7, fd12::/32");

    assert!(local_only("http://10.77.1.2:11434/v1"));
    assert!(local_only("https://10.77.255.255/"));
    assert!(local_only("http://100.100.0.7:8080/"));
    assert!(local_only("http://[fd12::5]:8080/"));
    assert!(local_only("ws://10.77.1.2:9000/socket"));

    // Neighbouring addresses, other private ranges and public hosts stay denied.
    assert!(!local_only("http://10.78.0.1/"));
    assert!(!local_only("http://100.100.0.8/"));
    assert!(!local_only("http://[fd13::1]/"));
    assert!(!local_only("http://192.168.50.1/"));
    assert!(!local_only("https://8.8.8.8/"));
    assert!(!local_only("https://example.com/"));
    // A host name is never matched, even one that embeds a listed address.
    assert!(!local_only("http://10.77.1.2.example.com/"));
    assert!(!local_only("http://intranet.local/"));
}

#[test]
fn online_mode_is_unaffected_by_the_list() {
    let _list = AllowedList::set("");
    let url = Url::parse("https://example.com/").unwrap();
    assert!(check_url_for_mode(ServicesMode::Online, &url, "test").is_ok());
}

#[test]
fn child_environment_parses_mode_and_hosts() {
    let (mode, hosts) = parse_env(Some("local_only"), "10.77.0.0/16, 100.100.0.7").unwrap();
    assert_eq!(mode, Some(ServicesMode::LocalOnly));
    assert_eq!(hosts.len(), 2);

    let (mode, hosts) = parse_env(Some("online"), "").unwrap();
    assert_eq!(mode, Some(ServicesMode::Online));
    assert!(hosts.is_empty());

    // Absent leaves the default alone; an unrecognized value fails closed.
    assert_eq!(parse_env(None, "").unwrap().0, None);
    assert_eq!(
        parse_env(Some("typo"), "").unwrap().0,
        Some(ServicesMode::LocalOnly)
    );

    // An invalid list is an error, not a partial application.
    assert!(parse_env(Some("local_only"), "10.0.0.1, 8.8.8.8").is_err());
}

#[test]
fn denial_in_chain_finds_a_denial_behind_other_errors() {
    #[derive(Debug, thiserror::Error)]
    #[error("transport failed")]
    struct Wrapper(#[source] crate::NetworkPolicyDenied);

    let denied = crate::NetworkPolicyDenied::new("redirect target", "https://example.com/");
    let found = crate::denial_in_chain(&Wrapper(denied.clone())).expect("denial in the chain");
    assert_eq!(found, denied);
    // The error itself counts too.
    assert_eq!(crate::denial_in_chain(&denied), Some(denied));

    let unrelated = std::io::Error::other("connection refused");
    assert_eq!(crate::denial_in_chain(&unrelated), None);
}
