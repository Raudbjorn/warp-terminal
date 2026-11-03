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
        network_policy::set_allowed_private_hosts(Vec::new());
    }
}

fn health(policy: Option<network_policy::PolicyReport>) -> LocalMultiAgentHealthResponse {
    LocalMultiAgentHealthResponse {
        ok: true,
        version: Some("test".to_owned()),
        network_policy: policy,
    }
}

fn report(
    services_mode: network_policy::ServicesMode,
    hosts: &[&str],
) -> network_policy::PolicyReport {
    network_policy::PolicyReport {
        services_mode,
        allowed_private_hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
    }
}

fn root_url() -> Url {
    Url::parse("http://127.0.0.1:8787").unwrap()
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

#[test]
fn health_response_without_a_policy_report_still_parses() {
    let old: LocalMultiAgentHealthResponse =
        serde_json::from_str(r#"{"ok":true,"version":"1","configHash":null}"#).unwrap();
    assert_eq!(old.network_policy, None);

    let current: LocalMultiAgentHealthResponse = serde_json::from_str(
        r#"{"ok":true,"version":"1","networkPolicy":{"servicesMode":"local_only","allowedPrivateHosts":["10.88.0.0/16"]}}"#,
    )
    .unwrap();
    assert_eq!(
        current.network_policy,
        Some(report(
            network_policy::ServicesMode::LocalOnly,
            &["10.88.0.0/16"]
        ))
    );
}

#[test]
fn a_running_service_is_reused_only_if_it_enforces_the_same_policy() {
    let _local_only = LocalOnlyGuard::enter();
    network_policy::set_allowed_private_hosts(
        network_policy::parse_allowed_private_hosts("10.88.0.0/16").unwrap(),
    );

    let matching = health(Some(report(
        network_policy::ServicesMode::LocalOnly,
        &["10.88.0.0/16"],
    )));
    ensure_service_policy_matches(&root_url(), &matching).unwrap();

    for (name, service) in [
        ("no report", health(None)),
        (
            "online",
            health(Some(report(
                network_policy::ServicesMode::Online,
                &["10.88.0.0/16"],
            ))),
        ),
        (
            "allow-list missing a host",
            health(Some(report(network_policy::ServicesMode::LocalOnly, &[]))),
        ),
        (
            "allow-list with an extra host",
            health(Some(report(
                network_policy::ServicesMode::LocalOnly,
                &["10.88.0.0/16", "100.100.0.7"],
            ))),
        ),
    ] {
        let error = ensure_service_policy_matches(&root_url(), &service).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("http://127.0.0.1:8787")
                && message.contains(network_policy::ENV_SERVICES_MODE)
                && message.contains(network_policy::ENV_ALLOWED_PRIVATE_HOSTS),
            "{name}: {message}"
        );
    }
}

/// A manager that has not started a service yet.
fn test_manager() -> LocalMultiAgentManager {
    LocalMultiAgentManager {
        config: LocalMultiAgentConfig::default(),
        status: LocalMultiAgentStatus::Starting,
        test_status: LocalMultiAgentTestStatus::NotRun,
        discovered_models: Vec::new(),
        child: None,
        restart_debounce: None,
        openai_api_key: None,
        applied_private_hosts: Vec::new(),
        original_server_root_url: None,
        failed_with_hosts: None,
    }
}

#[test]
fn the_applied_policy_is_the_snapshot_the_service_was_given() {
    let _local_only = LocalOnlyGuard::enter();
    let mut manager = test_manager();
    let started_with = network_policy::parse_allowed_private_hosts("10.88.0.0/16").unwrap();
    network_policy::set_allowed_private_hosts(started_with.clone());

    // The service enforces the list it was handed, and the allow-list still matches it.
    assert!(!manager.record_applied_private_hosts(started_with.clone()));
    assert_eq!(manager.applied_private_hosts, started_with);

    // The allow-list changed while the service was starting: it enforces the old list, so it
    // must be restarted with the current one.
    network_policy::set_allowed_private_hosts(Vec::new());
    assert!(manager.record_applied_private_hosts(started_with.clone()));
    // The recorded list is the snapshot the service was given, not the current allow-list.
    assert_eq!(manager.applied_private_hosts, started_with);
}

#[test]
fn a_service_that_failed_validation_publishes_no_root_url() {
    let _local_only = LocalOnlyGuard::enter();
    let mut manager = test_manager();

    // Nothing has been validated, so there is no local service to talk to.
    set_global_root_url(None);
    assert_eq!(local_no_cloud_root_url(), None);

    // A validated service is published.
    set_global_root_url(Some(root_url()));
    assert_eq!(local_no_cloud_root_url(), Some(root_url()));

    // A service that failed is not usable, so it must not stay published.
    manager.set_failed("the running service enforces a different policy".to_string());
    assert_eq!(local_no_cloud_root_url(), None);
}

#[test]
fn a_revoked_service_is_unpublished_until_its_replacement_is_healthy() {
    let _local_only = LocalOnlyGuard::enter();
    let mut manager = test_manager();
    set_global_root_url(Some(root_url()));
    assert_eq!(local_no_cloud_root_url(), Some(root_url()));

    // The allow-list lost a host: requests stop at once, not after the restart debounce.
    manager.revoke_running_service();
    assert_eq!(local_no_cloud_root_url(), None);
}

#[test]
fn unpublishing_gives_back_the_original_server_root_url() {
    let _local_only = LocalOnlyGuard::enter();
    let mut manager = test_manager();
    manager.original_server_root_url = Some("https://app.example.test".to_string());
    set_global_root_url(Some(root_url()));

    // Every route goes, including the process-wide one other clients read: the remembered
    // original is consumed by the restore, so a later publish records a fresh one.
    manager.unpublish_service_url();
    assert_eq!(local_no_cloud_root_url(), None);
    assert_eq!(manager.original_server_root_url, None);
}

#[test]
fn a_service_refused_for_its_policy_is_retried_when_the_allow_list_is_restored() {
    let _local_only = LocalOnlyGuard::enter();
    let mut manager = test_manager();
    let original = network_policy::parse_allowed_private_hosts("10.77.0.0/16").unwrap();
    let changed = network_policy::parse_allowed_private_hosts("10.88.0.0/16").unwrap();

    // A service validated against the original list is running.
    network_policy::set_allowed_private_hosts(original.clone());
    manager.applied_private_hosts = original.clone();
    assert!(!manager.allow_list_needs_restart());

    // The list changes and the replacement is refused for its policy, so nothing is running.
    network_policy::set_allowed_private_hosts(changed.clone());
    manager.set_failed("the running service enforces a different policy".to_string());
    assert_eq!(manager.failed_with_hosts, Some(changed));

    // Restoring the original list makes it equal to the last validated one, which alone looks
    // like no change. The failure was under another list, so the service is tried again.
    network_policy::set_allowed_private_hosts(original);
    assert!(!policy_needs_restart(&manager.applied_private_hosts));
    assert!(manager.allow_list_needs_restart());

    // A failure under the current list is not retried by unrelated setting changes.
    manager.set_failed("still refused".to_string());
    assert!(!manager.allow_list_needs_restart());
}
