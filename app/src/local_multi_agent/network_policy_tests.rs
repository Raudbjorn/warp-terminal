//! The clients in this module follow the network policy. Each test sets the process-wide services
//! mode to local-only for its duration, so they run `#[serial]`. The URLs they use
//! are refused before any connection is made.

use super::*;
use crate::ai::predict::local_command_autocomplete::{
    LocalCommandAutocompleteProviderSettings, LocalCommandAutocompleteRequest,
    request_local_command_autocomplete,
};

/// Sets local-only mode for a test and puts back what it found. The mode and the allow-list are
/// process-wide and other app tests change them too, so every test that uses this is marked
/// `#[serial]`, the same lock those use; a lock of this module's own would not exclude them.
struct LocalOnlyGuard {
    mode: network_policy::ServicesMode,
    hosts: Vec<network_policy::PrivateNetwork>,
}

impl LocalOnlyGuard {
    fn enter() -> Self {
        let guard = Self {
            mode: network_policy::services_mode(),
            hosts: network_policy::allowed_private_hosts(),
        };
        network_policy::set_services_mode(network_policy::ServicesMode::LocalOnly);
        guard
    }
}

impl Drop for LocalOnlyGuard {
    fn drop(&mut self) {
        network_policy::set_services_mode(self.mode);
        network_policy::set_allowed_private_hosts(std::mem::take(&mut self.hosts));
        // Tests publish endpoints only through these app-side sources; leave none behind.
        for source in [
            network_policy::EndpointSource::AiSettings,
            network_policy::EndpointSource::ProfileSettings,
        ] {
            network_policy::set_configured_endpoints(source, std::iter::empty());
        }
    }
}

/// An allow-list with the given private hosts and no configured endpoints.
fn hosts_only(private_hosts: Vec<network_policy::PrivateNetwork>) -> network_policy::AllowList {
    network_policy::AllowList {
        private_hosts,
        endpoints: Default::default(),
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
        allowed_endpoints: Vec::new(),
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
#[serial_test::serial]
async fn model_list_of_a_public_provider_is_refused() {
    let _local_only = LocalOnlyGuard::enter();
    let error = fetch_provider_models("https://api.example.com/v1", Some("sk-secret"))
        .await
        .unwrap_err();
    assert_denied(&error);
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn a_refused_model_list_never_repeats_the_provider_url() {
    let _local_only = LocalOnlyGuard::enter();
    let error = fetch_provider_models("https://user:hunter2@api.example.com/v1?key=hunter2", None)
        .await
        .unwrap_err();

    // Still the typed denial, so callers can tell a refusal from an outage.
    assert_denied(&error);
    // The settings page renders this error with `{error:#}`.
    let shown = format!("{error:#}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(!shown.contains("api.example.com"), "{shown}");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn model_list_of_a_private_provider_is_refused_unless_it_is_allowed() {
    let _local_only = LocalOnlyGuard::enter();
    // 10.77.0.0/16 is not in anyone's allowed list in this process.
    let error = fetch_provider_models("http://10.77.1.2:11434/v1", None)
        .await
        .unwrap_err();
    assert_denied(&error);
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn health_check_of_a_public_service_is_refused() {
    let _local_only = LocalOnlyGuard::enter();
    let error = health_check(Url::parse("https://service.example.com").unwrap())
        .await
        .unwrap_err();
    assert_denied(&error);
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
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
#[serial_test::serial]
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
        applied_allow_list: network_policy::AllowList::default(),
        original_server_root_url: None,
        failed_with_allow_list: None,
    }
}

#[test]
#[serial_test::serial]
fn the_applied_policy_is_the_snapshot_the_service_was_given() {
    let _local_only = LocalOnlyGuard::enter();
    let mut manager = test_manager();
    let started_with = network_policy::parse_allowed_private_hosts("10.88.0.0/16").unwrap();
    network_policy::set_allowed_private_hosts(started_with.clone());

    // The service enforces the list it was handed, and the allow-list still matches it.
    assert!(!manager.record_applied_allow_list(hosts_only(started_with.clone())));
    assert_eq!(manager.applied_allow_list, hosts_only(started_with.clone()));

    // The allow-list changed while the service was starting: it enforces the old list, so it
    // must be restarted with the current one.
    network_policy::set_allowed_private_hosts(Vec::new());
    assert!(manager.record_applied_allow_list(hosts_only(started_with.clone())));
    // The recorded list is the snapshot the service was given, not the current allow-list.
    assert_eq!(manager.applied_allow_list, hosts_only(started_with.clone()));
}

#[test]
#[serial_test::serial]
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
#[serial_test::serial]
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
#[serial_test::serial]
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
#[serial_test::serial]
fn a_service_refused_for_its_policy_is_retried_when_the_allow_list_is_restored() {
    let _local_only = LocalOnlyGuard::enter();
    let mut manager = test_manager();
    let original = network_policy::parse_allowed_private_hosts("10.77.0.0/16").unwrap();
    let changed = network_policy::parse_allowed_private_hosts("10.88.0.0/16").unwrap();

    // A service validated against the original list is running.
    network_policy::set_allowed_private_hosts(original.clone());
    manager.applied_allow_list = hosts_only(original.clone());
    assert!(!manager.allow_list_needs_restart());

    // The list changes and the replacement is refused for its policy, so nothing is running.
    network_policy::set_allowed_private_hosts(changed.clone());
    manager.set_failed("the running service enforces a different policy".to_string());
    assert_eq!(manager.failed_with_allow_list, Some(hosts_only(changed)));

    // While the list is still the one the failure happened under, unrelated setting changes do
    // not restart it, even though it differs from the last validated list.
    assert!(policy_needs_restart(&manager.applied_allow_list));
    assert!(!manager.allow_list_needs_restart());

    // Restoring the original list makes it equal to the last validated one, which alone looks
    // like no change. The failure was under another list, so the service is tried again.
    network_policy::set_allowed_private_hosts(original);
    assert!(!policy_needs_restart(&manager.applied_allow_list));
    assert!(manager.allow_list_needs_restart());

    // A failure under the current list is not retried by unrelated setting changes.
    manager.set_failed("still refused".to_string());
    assert!(!manager.allow_list_needs_restart());
}

#[test]
#[serial_test::serial]
fn a_spawned_service_is_published_only_if_it_enforces_the_policy_it_was_given() {
    let _local_only = LocalOnlyGuard::enter();
    let binary = Path::new("target/debug/local_multi_agent_service");
    let given = hosts_only(network_policy::parse_allowed_private_hosts("10.77.0.0/16").unwrap());

    // A build from before the policy existed reports nothing, or reports online mode: it
    // ignored the environment it was started with.
    for stale in [
        health(None),
        health(Some(report(
            network_policy::ServicesMode::Online,
            &["10.77.0.0/16"],
        ))),
    ] {
        let error = ensure_spawned_service_policy(binary, &given, &stale).unwrap_err();
        assert!(error.to_string().contains("does not enforce"), "{error:#}");
    }

    // One that enforces what it was handed is accepted, even if the allow-list changed while it
    // was starting: that is the caller's restart to make, not a service enforcing the wrong policy.
    let enforcing = health(Some(report(
        network_policy::ServicesMode::LocalOnly,
        &["10.77.0.0/16"],
    )));
    network_policy::set_allowed_private_hosts(Vec::new());
    assert!(ensure_spawned_service_policy(binary, &given, &enforcing).is_ok());

    // One that enforces a different list than it was handed is refused.
    let other = health(Some(report(
        network_policy::ServicesMode::LocalOnly,
        &["10.88.0.0/16"],
    )));
    assert!(ensure_spawned_service_policy(binary, &given, &other).is_err());
}

#[test]
#[serial_test::serial]
fn a_changed_configured_endpoint_restarts_the_service() {
    let _local_only = LocalOnlyGuard::enter();
    let mut manager = test_manager();
    network_policy::set_configured_endpoints(
        network_policy::EndpointSource::AiSettings,
        ["https://api.minimax.io/v1"],
    );
    manager.applied_allow_list = network_policy::AllowList::current();
    assert!(!manager.allow_list_needs_restart());

    // The user points the base URL at another provider: the running service still allows the
    // old host and not the new one, so it has to be restarted.
    network_policy::set_configured_endpoints(
        network_policy::EndpointSource::AiSettings,
        ["https://api.openai.com/v1"],
    );
    assert!(manager.allow_list_needs_restart());
}

#[test]
#[serial_test::serial]
fn a_spawned_service_with_other_configured_endpoints_is_refused() {
    let _local_only = LocalOnlyGuard::enter();
    let binary = Path::new("target/debug/local_multi_agent_service");
    network_policy::set_configured_endpoints(
        network_policy::EndpointSource::AiSettings,
        ["https://api.minimax.io/v1"],
    );
    let given = network_policy::AllowList::current();

    let mut matching = report(network_policy::ServicesMode::LocalOnly, &[]);
    matching.allowed_endpoints = vec!["https://api.minimax.io:443".to_owned()];
    assert!(ensure_spawned_service_policy(binary, &given, &health(Some(matching))).is_ok());

    // A service from before configured endpoints reports none, so it would refuse the provider.
    let without = report(network_policy::ServicesMode::LocalOnly, &[]);
    assert!(ensure_spawned_service_policy(binary, &given, &health(Some(without))).is_err());
}

#[test]
#[serial_test::serial]
fn configured_https_provider_urls_pass_the_policy_and_others_do_not() {
    let _local_only = LocalOnlyGuard::enter();
    network_policy::set_configured_endpoints(
        network_policy::EndpointSource::AiSettings,
        ["https://api.minimax.io/v1"],
    );
    let check = |url: &str| network_policy::check_url(&Url::parse(url).unwrap(), "test");
    assert!(check("https://api.minimax.io/v1/chat/completions").is_ok());
    assert!(check("http://api.minimax.io/v1/chat/completions").is_err());
    assert!(check("https://api.openai.com/v1/chat/completions").is_err());
    // Loopback http keeps working without configuration.
    assert!(check("http://127.0.0.1:1234/v1/models").is_ok());
}

#[test]
fn profile_endpoint_urls_prefers_the_default_profile_over_legacy_fields() {
    let mut keys = ::ai::api_keys::ApiKeys {
        openai_base_url: Some("https://legacy.example/v1".to_owned()),
        ..Default::default()
    };
    keys.profile_inference_settings.insert(
        "default".to_owned(),
        ::ai::api_keys::ProfileInferenceSettings {
            openai_base_url: Some("https://profile.example/v1".to_owned()),
            ..Default::default()
        },
    );
    let urls = profile_endpoint_urls(&keys, &[]);
    assert!(!urls.contains(&"https://legacy.example/v1".to_owned()));
    assert!(urls.contains(&"https://profile.example/v1".to_owned()));
}

fn custom_endpoint(url: &str) -> ::ai::api_keys::CustomEndpoint {
    ::ai::api_keys::CustomEndpoint {
        url: url.to_owned(),
        ..Default::default()
    }
}

#[test]
fn profile_endpoint_urls_reads_the_legacy_base_url_until_a_default_profile_exists() {
    let keys = ::ai::api_keys::ApiKeys {
        openai_base_url: Some("https://legacy.example/v1".to_owned()),
        ..Default::default()
    };
    assert_eq!(
        profile_endpoint_urls(&keys, &[]),
        vec!["https://legacy.example/v1".to_owned()]
    );
}

#[test]
fn profile_endpoint_urls_takes_other_profiles_custom_endpoints_from_the_profile() {
    let mut keys = ::ai::api_keys::ApiKeys::default();
    keys.profile_inference_settings.insert(
        "work".to_owned(),
        ::ai::api_keys::ProfileInferenceSettings {
            custom_endpoints: vec![custom_endpoint("https://work.example/v1")],
            ..Default::default()
        },
    );
    let urls = profile_endpoint_urls(&keys, &[custom_endpoint("https://active.example/v1")]);
    assert!(urls.contains(&"https://work.example/v1".to_owned()));
    assert!(urls.contains(&"https://active.example/v1".to_owned()));
}

/// Migration to `agents.custom_endpoints` leaves the legacy copies in `ApiKeys`. Once the user
/// removes the migrated endpoint, the active list is empty and its host must be denied again.
#[test]
#[serial_test::serial]
fn a_removed_migrated_custom_endpoint_is_no_longer_allowed() {
    let _local_only = LocalOnlyGuard::enter();
    let migrated = custom_endpoint("https://migrated.example/v1");
    let mut keys = ::ai::api_keys::ApiKeys {
        custom_endpoints: vec![migrated.clone()],
        ..Default::default()
    };
    keys.profile_inference_settings.insert(
        ::ai::api_keys::DEFAULT_PROFILE_INFERENCE_KEY.to_owned(),
        ::ai::api_keys::ProfileInferenceSettings {
            custom_endpoints: vec![migrated.clone()],
            ..Default::default()
        },
    );
    let check = |url: &str| network_policy::check_url(&Url::parse(url).unwrap(), "test");
    let publish = |urls: Vec<String>| {
        network_policy::set_configured_endpoints(
            network_policy::EndpointSource::ProfileSettings,
            urls.iter().map(String::as_str),
        );
    };

    publish(profile_endpoint_urls(
        &keys,
        std::slice::from_ref(&migrated),
    ));
    assert!(check("https://migrated.example/v1/chat/completions").is_ok());

    publish(profile_endpoint_urls(&keys, &[]));
    assert!(check("https://migrated.example/v1/chat/completions").is_err());
}
