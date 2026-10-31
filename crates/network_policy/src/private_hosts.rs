//! User-controlled opt-in for private-network hosts in local-only mode.
//!
//! Local-only mode allows loopback and nothing else. A common local-AI setup is a model server on
//! another machine on the LAN or on a tailnet, so the user can additionally allow specific
//! private addresses or ranges. The list is empty by default, which keeps loopback-only behavior.
//!
//! Only IP literals and CIDR ranges are matched, and only inside the private ranges in
//! [`PRIVATE_RANGES`]. Host names are never matched: a name says nothing about the address it
//! resolves to, so allowing one would invite DNS rebinding. Allowing a public range such as
//! `0.0.0.0/0` is rejected for the same reason: it would turn local-only mode off.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::RwLock;

use url::Url;

use crate::ServicesMode;

/// Environment variable carrying the services mode to child processes (`local_only` / `online`).
pub const ENV_SERVICES_MODE: &str = "WARP_SERVICES_MODE";
/// Environment variable carrying the allowed private hosts to child processes, comma separated.
pub const ENV_ALLOWED_PRIVATE_HOSTS: &str = "WARP_ALLOWED_PRIVATE_HOSTS";

/// The ranges an allowed entry must fall inside: RFC 1918, CGNAT (Tailscale), IPv4 link-local, IPv6
/// unique-local and IPv6 link-local.
const PRIVATE_RANGES: [(IpAddr, u8); 7] = [
    (IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0)), 8),
    (IpAddr::V4(Ipv4Addr::new(172, 16, 0, 0)), 12),
    (IpAddr::V4(Ipv4Addr::new(192, 168, 0, 0)), 16),
    (IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0)), 10),
    (IpAddr::V4(Ipv4Addr::new(169, 254, 0, 0)), 16),
    (IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0)), 7),
    (IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0)), 10),
];

static ALLOWED_PRIVATE_HOSTS: RwLock<Vec<PrivateNetwork>> = RwLock::new(Vec::new());

/// A single allowed entry: an IP address (a /32 or /128) or a CIDR range inside a private range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivateNetwork {
    /// The network address, with the host bits cleared.
    addr: IpAddr,
    prefix_len: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PrivateHostsError {
    #[error("`{0}` is not an IP address or CIDR range (host names cannot be allowed)")]
    Invalid(String),
    #[error(
        "`{0}` is not a private-network address; only 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, \
         100.64.0.0/10 (Tailscale), 169.254.0.0/16, fc00::/7 and fe80::/10 can be allowed"
    )]
    NotPrivate(String),
}

impl PrivateNetwork {
    /// Parses `10.0.0.5`, `192.168.1.0/24`, `fd00::/8` or `[fd00::1]`.
    pub fn parse(entry: &str) -> Result<Self, PrivateHostsError> {
        let invalid = || PrivateHostsError::Invalid(entry.to_owned());
        let trimmed = entry.trim();
        let (addr_part, prefix_part) = match trimmed.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (trimmed, None),
        };
        let addr_part = addr_part
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or(addr_part);
        let addr: IpAddr = addr_part.parse().map_err(|_| invalid())?;
        let max_len = max_prefix_len(&addr);
        let prefix_len = match prefix_part {
            Some(prefix) => prefix.parse::<u8>().map_err(|_| invalid())?,
            None => max_len,
        };
        if prefix_len > max_len {
            return Err(invalid());
        }

        let network = Self {
            addr: mask(addr, prefix_len),
            prefix_len,
        };
        if network.is_within_private_range() {
            Ok(network)
        } else {
            Err(PrivateHostsError::NotPrivate(entry.trim().to_owned()))
        }
    }

    /// Whether `ip` is inside this entry. IPv4-mapped IPv6 addresses are compared as IPv4.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = normalize(ip);
        same_family(&self.addr, &ip) && mask(ip, self.prefix_len) == self.addr
    }

    fn is_within_private_range(&self) -> bool {
        PRIVATE_RANGES.iter().any(|(range_addr, range_len)| {
            same_family(&self.addr, range_addr)
                && *range_len <= self.prefix_len
                && mask(self.addr, *range_len) == *range_addr
        })
    }
}

impl std::fmt::Display for PrivateNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.prefix_len == max_prefix_len(&self.addr) {
            write!(f, "{}", self.addr)
        } else {
            write!(f, "{}/{}", self.addr, self.prefix_len)
        }
    }
}

/// Parses a user-entered list. Entries are separated by commas, semicolons or whitespace, and an
/// empty list is valid. The first invalid entry fails the whole list, so a typo never silently
/// leaves part of the list unapplied.
pub fn parse_allowed_private_hosts(input: &str) -> Result<Vec<PrivateNetwork>, PrivateHostsError> {
    input
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|entry| !entry.is_empty())
        .map(PrivateNetwork::parse)
        .collect()
}

/// Replaces the process-wide list of allowed private hosts.
pub fn set_allowed_private_hosts(networks: Vec<PrivateNetwork>) {
    *ALLOWED_PRIVATE_HOSTS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = networks;
}

pub fn allowed_private_hosts() -> Vec<PrivateNetwork> {
    ALLOWED_PRIVATE_HOSTS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Whether `url` targets an IP literal on the allowed list. Host names never match.
pub(crate) fn is_allowed_private_url(url: &Url) -> bool {
    let ip = match url.host() {
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip),
        Some(url::Host::Domain(_)) | None => return false,
    };
    ALLOWED_PRIVATE_HOSTS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|network| network.contains(ip))
}

/// The environment a child process needs to enforce the same policy as this process. Pass it to
/// the child's `Command` and call [`init_from_env`] at the start of the child's `main`.
pub fn env_for_child() -> Vec<(&'static str, String)> {
    let mode = match crate::services_mode() {
        ServicesMode::LocalOnly => "local_only",
        ServicesMode::Online => "online",
    };
    let hosts = allowed_private_hosts()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    vec![
        (ENV_SERVICES_MODE, mode.to_owned()),
        (ENV_ALLOWED_PRIVATE_HOSTS, hosts),
    ]
}

/// Applies the policy passed down by a parent through [`env_for_child`]. An absent
/// `WARP_SERVICES_MODE` leaves the default (online); any value other than `online` is treated as
/// local-only, so a typo fails closed. An invalid host list is an error rather than being
/// partially applied, and nothing is changed in that case.
pub fn init_from_env() -> Result<(), PrivateHostsError> {
    let mode = std::env::var(ENV_SERVICES_MODE).ok();
    let hosts = std::env::var(ENV_ALLOWED_PRIVATE_HOSTS).unwrap_or_default();
    let (mode, hosts) = parse_env(mode.as_deref(), &hosts)?;
    if let Some(mode) = mode {
        crate::set_services_mode(mode);
    }
    set_allowed_private_hosts(hosts);
    Ok(())
}

fn parse_env(
    mode: Option<&str>,
    hosts: &str,
) -> Result<(Option<ServicesMode>, Vec<PrivateNetwork>), PrivateHostsError> {
    let hosts = parse_allowed_private_hosts(hosts)?;
    let mode = mode.map(|mode| {
        if mode.trim().eq_ignore_ascii_case("online") {
            ServicesMode::Online
        } else {
            ServicesMode::LocalOnly
        }
    });
    Ok((mode, hosts))
}

fn max_prefix_len(addr: &IpAddr) -> u8 {
    match addr {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

fn same_family(a: &IpAddr, b: &IpAddr) -> bool {
    a.is_ipv4() == b.is_ipv4()
}

fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        other => other,
    }
}

/// Clears all but the first `prefix_len` bits of `addr`.
fn mask(addr: IpAddr, prefix_len: u8) -> IpAddr {
    match addr {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let masked = match prefix_len {
                0 => 0,
                len => bits & (u32::MAX << (32 - u32::from(len))),
            };
            IpAddr::V4(Ipv4Addr::from(masked))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let masked = match prefix_len {
                0 => 0,
                len => bits & (u128::MAX << (128 - u32::from(len))),
            };
            IpAddr::V6(Ipv6Addr::from(masked))
        }
    }
}

#[cfg(test)]
#[path = "private_hosts_tests.rs"]
mod tests;
