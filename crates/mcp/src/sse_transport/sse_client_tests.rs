use super::*;

#[test]
fn a_policy_denial_reports_the_purpose_and_never_the_url() {
    let denied = network_policy::NetworkPolicyDenied::new(
        "MCP SSE stream",
        "https://user:hunter2@mcp.example.com/sse?token=hunter2",
    );

    let message = SseTransportError::<std::io::Error>::from(denied).to_string();

    assert!(message.contains("MCP SSE stream"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
    assert!(!message.contains("mcp.example.com"), "{message}");
}
