//! The `skip_login` build refuses every authenticated request on purpose: this fork runs without a
//! Warp account, so there is no token to send. These tests pin that, since `session_tests.rs`
//! only compiles without the feature, and the app enables it by default.
#![cfg(feature = "skip_login")]

use std::sync::Arc;

use futures::executor::block_on;
use warp_server_auth::auth_state::AuthState;
use warp_server_auth::credentials::Credentials;

use super::AuthSession;

fn session_with_credentials(credentials: Option<Credentials>) -> AuthSession {
    let auth_state = Arc::new(AuthState::new_logged_out_for_test());
    auth_state.set_credentials(credentials);
    let (event_sender, _event_receiver) = async_channel::unbounded();
    AuthSession::new(
        Arc::new(http_client::Client::new()),
        auth_state,
        event_sender,
    )
}

#[test]
fn authenticated_requests_fail_whatever_the_credentials() {
    for credentials in [
        None,
        Some(Credentials::Bearer("daemon-token".to_string())),
        Some(Credentials::ApiKey {
            key: "api-key".to_string(),
            owner_type: None,
        }),
    ] {
        let session = session_with_credentials(credentials.clone());

        let err = block_on(session.get_or_refresh_access_token())
            .expect_err("skip_login must refuse authenticated requests");

        assert!(
            err.to_string().contains("skip_login enabled"),
            "unexpected error for {credentials:?}: {err}"
        );
    }
}
