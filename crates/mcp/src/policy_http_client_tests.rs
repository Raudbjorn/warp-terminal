use network_policy::ServicesMode;

use super::*;

type Check = Result<(), StreamableHttpError<io::Error>>;

fn check(mode: ServicesMode, uri: &str) -> Check {
    check_request(mode, uri)
}

#[test]
fn online_mode_allows_any_host() {
    assert!(check(ServicesMode::Online, "https://mcp.example.com/mcp").is_ok());
}

#[test]
fn local_only_mode_allows_loopback_and_refuses_public_hosts() {
    assert!(check(ServicesMode::LocalOnly, "http://127.0.0.1:8123/mcp").is_ok());
    assert!(check(ServicesMode::LocalOnly, "https://mcp.example.com/mcp").is_err());
}

#[test]
fn a_refusal_names_the_rule_and_never_the_url() {
    let url = "https://user:hunter2@mcp.example.com/mcp?token=hunter2";
    let Err(error) = check(ServicesMode::LocalOnly, url) else {
        panic!("a public host must be refused in local-only mode");
    };
    let message = error.to_string();
    assert!(!message.contains("hunter2"), "{message}");
    assert!(!message.contains("mcp.example.com"), "{message}");
    assert!(message.contains("network policy"), "{message}");
}
