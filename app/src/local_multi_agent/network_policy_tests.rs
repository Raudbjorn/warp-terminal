//! The clients in this module follow the network policy. Each test sets the process-wide services
//! mode to local-only for its duration, so they all hold [`LOCAL_ONLY_LOCK`]. The URLs they use
//! are refused before any connection is made.

use std::sync::{Mutex, MutexGuard, PoisonError};

use super::*;
use crate::ai::predict::local_command_autocomplete::{
    LocalCommandAutocompleteProviderSettings, LocalCommandAutocompleteRequest,
    request_local_command_autocomplete,
};

static LOCAL_ONLY_LOCK: Mutex<()> = Mutex::new(());

struct LocalOnlyGuard(#[allow(dead_code)] MutexGuard<'static, ()>);

impl LocalOnlyGuard {
    fn enter() -> Self {
        let guard = LOCAL_ONLY_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        network_policy::set_services_mode(network_policy::ServicesMode::LocalOnly);
        Self(guard)
    }
}

impl Drop for LocalOnlyGuard {
    fn drop(&mut self) {
        network_policy::set_services_mode(network_policy::ServicesMode::Online);
    }
}

fn assert_denied(error: &anyhow::Error) {
    assert!(
        error
            .chain()
            .any(|cause| cause.is::<network_policy::NetworkPolicyDenied>()),
        "expected a network policy denial, got: {error:#}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_list_of_a_public_provider_is_refused() {
    let _local_only = LocalOnlyGuard::enter();
    let error = fetch_provider_models("https://api.example.com/v1", Some("sk-secret"))
        .await
        .unwrap_err();
    assert_denied(&error);
}

#[tokio::test(flavor = "current_thread")]
async fn model_list_of_a_private_provider_is_refused_unless_it_is_allowed() {
    let _local_only = LocalOnlyGuard::enter();
    // 10.77.0.0/16 is not in anyone's allowed list in this process.
    let error = fetch_provider_models("http://10.77.1.2:11434/v1", None)
        .await
        .unwrap_err();
    assert_denied(&error);
}

#[tokio::test(flavor = "current_thread")]
async fn health_check_of_a_public_service_is_refused() {
    let _local_only = LocalOnlyGuard::enter();
    let error = health_check(Url::parse("https://service.example.com").unwrap())
        .await
        .unwrap_err();
    assert_denied(&error);
}

#[tokio::test(flavor = "current_thread")]
async fn command_autocomplete_to_a_public_service_is_refused() {
    let _local_only = LocalOnlyGuard::enter();
    let error = request_local_command_autocomplete(
        Url::parse("https://service.example.com").unwrap(),
        &LocalCommandAutocompleteRequest {
            prefix: "git ".to_owned(),
            ..Default::default()
        },
        LocalCommandAutocompleteProviderSettings {
            openai_api_key: Some("sk-secret".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_denied(&error);
}
