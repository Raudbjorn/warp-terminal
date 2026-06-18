use warpui::r#async::BoxFuture;

use super::*;

fn static_auth_context() -> Arc<RemoteServerAuthContext> {
    Arc::new(RemoteServerAuthContext::new(
        || -> BoxFuture<'static, Option<String>> { Box::pin(async { None }) },
        || "user id/with spaces".to_string(),
        String::new(),
        String::new(),
        true,
    ))
}

#[test]
fn remote_proxy_command_quotes_identity_key() {
    let transport = SshTransport::new(
        PathBuf::from("/tmp/control-master.sock"),
        static_auth_context(),
        true,
    );

    let command = transport.remote_proxy_command();

    assert!(command.contains("remote-server-proxy --identity-key"));
    assert!(command.contains("'user id/with spaces'"));
}

#[test]
fn transport_does_not_trust_tagged_server_for_untagged_client_by_default() {
    let transport = SshTransport::new(
        PathBuf::from("/tmp/control-master.sock"),
        static_auth_context(),
        InstallScriptOptions::new(
            remote_server::setup::default_download_base_url().to_string(),
            remote_server::setup::default_download_channel().to_string(),
        ),
    );

    assert!(!transport.allow_tagged_server_with_untagged_client());
}

#[test]
fn transport_can_trust_tagged_server_after_user_approved_install() {
    let transport = SshTransport::new(
        PathBuf::from("/tmp/control-master.sock"),
        static_auth_context(),
        InstallScriptOptions::new(
            remote_server::setup::default_download_base_url().to_string(),
            remote_server::setup::default_download_channel().to_string(),
        ),
    )
    .with_tagged_server_untagged_client_trust();

    assert!(transport.allow_tagged_server_with_untagged_client());
}

#[test]
fn parse_remote_server_version_from_oz_output() {
    assert_eq!(
        parse_remote_server_version("Oz v0.2026.05.20.09.21.stable_03\n"),
        Some("v0.2026.05.20.09.21.stable_03")
    );
}

#[test]
fn parse_remote_server_version_ignores_plain_version_word() {
    assert_eq!(parse_remote_server_version("Oz version dev\n"), None);
}

#[test]
fn parse_remote_server_protocol_version_from_output() {
    assert_eq!(
        parse_remote_server_protocol_version(
            "remote-server-protocol-2026-06-session-scoped-envelope\n"
        ),
        Some("remote-server-protocol-2026-06-session-scoped-envelope")
    );
}

#[test]
fn protocol_check_rejects_missing_protocol_output() {
    let result = classify_protocol_check_success("");

    assert!(matches!(
        result,
        Err(BinaryUpdateReason::ProtocolMismatch { found: None, .. })
    ));
}

#[test]
fn protocol_check_accepts_current_protocol_output() {
    assert!(classify_protocol_check_success(
        remote_server::protocol::REMOTE_SERVER_PROTOCOL_VERSION
    )
    .is_ok());
}

#[test]
fn binary_check_accepts_matching_release_version() {
    let status = classify_binary_check_success_with_client_version(
        "Oz v0.2026.06.05.10.00.stable_01\n",
        Some("v0.2026.06.05.10.00.stable_01"),
    );

    assert_eq!(status, BinaryCheckStatus::Installed);
}

#[test]
fn binary_check_requires_update_for_mismatched_release_version() {
    let status = classify_binary_check_success_with_client_version(
        "Oz v0.2026.05.20.09.21.stable_03\n",
        Some("v0.2026.06.05.10.00.stable_01"),
    );

    assert!(matches!(
        status,
        BinaryCheckStatus::UpdateRequired {
            reason: BinaryUpdateReason::VersionMismatch { .. },
            installed_version: Some(_),
        }
    ));
}

#[test]
fn binary_check_requires_update_when_client_has_no_release_version() {
    let status = classify_binary_check_success_with_client_version(
        "Oz v0.2026.05.20.09.21.stable_03\n",
        None,
    );

    assert!(matches!(
        status,
        BinaryCheckStatus::UpdateRequired {
            reason: BinaryUpdateReason::MissingClientVersion,
            installed_version: Some(_),
        }
    ));
}

#[test]
fn binary_check_accepts_untagged_client_with_untagged_binary() {
    let status = classify_binary_check_success_with_client_version("Oz dev build\n", None);

    assert_eq!(status, BinaryCheckStatus::Installed);
}

#[test]
fn cleanup_daemons_command_covers_all_channel_dirs() {
    let command = cleanup_remote_daemons_command("user id/with spaces");

    assert!(command.contains("~/.warp/remote-server/"));
    assert!(command.contains("~/.warp-preview/remote-server/"));
    assert!(command.contains("~/.warp-dev/remote-server/"));
    assert!(command.contains("~/.warp-local/remote-server/"));
    assert!(command.contains("~/.slipstream/remote-server/"));
    assert!(command.contains("server*.pid"));
    assert!(command.contains("server*.sock"));
    assert!(command.contains("remote-server-daemon"));
}
