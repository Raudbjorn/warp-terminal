use std::time::{Duration, SystemTime};

use super::*;

fn make_manager(keys: ApiKeys) -> ApiKeyManager {
    make_manager_with_grok(keys, None)
}

fn make_manager_with_grok(keys: ApiKeys, grok_tokens: Option<GrokTokens>) -> ApiKeyManager {
    ApiKeyManager {
        keys,
        grok_tokens,
        #[cfg(not(target_family = "wasm"))]
        grok_refresh_allowed: false,
        #[cfg(not(target_family = "wasm"))]
        grok_refresh_in_flight: false,
        aws_credentials_state: AwsCredentialsState::Missing,
        aws_credentials_refresh_strategy: AwsCredentialsRefreshStrategy::default(),
        geap_credentials_state: GeapCredentialsState::Missing,
        secure_storage_write_version: 0,
        grok_secure_storage_write_version: 0,
    }
}

fn make_manager_with_geap(geap_credentials_state: GeapCredentialsState) -> ApiKeyManager {
    let mut manager = make_manager(ApiKeys::default());
    manager.geap_credentials_state = geap_credentials_state;
    manager
}

fn grok_tokens(access_token: &str, expires_in: Option<u64>) -> GrokTokens {
    GrokTokens {
        access_token: access_token.into(),
        refresh_token: Some("refresh".into()),
        expires_at: expires_in.map(|secs| SystemTime::now() + Duration::from_secs(secs)),
        connected_at: None,
    }
}

fn geap_credentials(access_token: &str, expires_in: Option<u64>) -> GeapCredentials {
    GeapCredentials::new(
        access_token.into(),
        expires_in.map(|secs| SystemTime::now() + Duration::from_secs(secs)),
    )
}

fn geap_binding() -> GeapMintBinding {
    GeapMintBinding {
        user_uid: "user-1".into(),
        audience:
            "//iam.googleapis.com/projects/1/locations/global/workloadIdentityPools/p/providers/q"
                .into(),
        federation: GeapFederation::ServiceAccount {
            email: "sa@proj.iam.gserviceaccount.com".into(),
        },
    }
}

// The expected binding the request build site passes in is the same type as
// the stored `minted_for`, so the attach check is a plain `==`.
fn geap_gate() -> GeapMintBinding {
    geap_binding()
}

fn geap_loaded(access_token: &str, expires_in: Option<u64>) -> GeapCredentialsState {
    GeapCredentialsState::Loaded {
        credentials: geap_credentials(access_token, expires_in),
        loaded_at: SystemTime::now(),
        minted_for: geap_binding(),
    }
}

fn endpoint(
    name: &str,
    url: &str,
    api_key: &str,
    models: &[(&str, Option<&str>)],
) -> CustomEndpoint {
    endpoint_with_keys(
        name,
        url,
        api_key,
        &models
            .iter()
            .enumerate()
            .map(|(i, (n, a))| (*n, *a, format!("cfg-{i}")))
            .collect::<Vec<_>>()
            .iter()
            .map(|(n, a, k)| (*n, *a, k.as_str()))
            .collect::<Vec<_>>(),
    )
}

fn endpoint_with_keys(
    name: &str,
    url: &str,
    api_key: &str,
    models: &[(&str, Option<&str>, &str)],
) -> CustomEndpoint {
    CustomEndpoint {
        name: name.into(),
        url: url.into(),
        api_key: api_key.into(),
        models: models
            .iter()
            .map(|(n, a, cfg)| CustomEndpointModel {
                name: (*n).into(),
                alias: a.map(|s| s.into()),
                config_key: (*cfg).into(),
            })
            .collect(),
    }
}

// ── serde round-trip ────────────────────────────────────────────

#[test]
fn serde_round_trip_empty() {
    let keys = ApiKeys::default();
    let json = serde_json::to_string(&keys).unwrap();
    let deser: ApiKeys = serde_json::from_str(&json).unwrap();
    assert_eq!(keys, deser);
}

#[test]
fn serde_round_trip_with_provider_keys() {
    let keys = ApiKeys {
        openai: Some("sk-openai".into()),
        anthropic: Some("sk-ant-abc".into()),
        google: Some("AIzaSy123".into()),
        open_router: Some("sk-or-xxx".into()),
        custom_endpoints: vec![],
        ..Default::default()
    };
    let json = serde_json::to_string(&keys).unwrap();
    let deser: ApiKeys = serde_json::from_str(&json).unwrap();
    assert_eq!(keys, deser);
}

#[test]
fn serde_round_trip_with_custom_endpoints() {
    let keys = ApiKeys {
        openai: None,
        anthropic: None,
        google: None,
        open_router: None,
        custom_endpoints: vec![
            endpoint("ep1", "https://a.io/v1", "key1", &[("gpt-4", Some("fast"))]),
            endpoint(
                "ep2",
                "https://b.io/v1",
                "key2",
                &[("llama-70b", None), ("mixtral", Some("mix"))],
            ),
        ],
        ..Default::default()
    };
    let json = serde_json::to_string(&keys).unwrap();
    let deser: ApiKeys = serde_json::from_str(&json).unwrap();
    assert_eq!(keys, deser);
}

#[test]
fn serde_ignores_unknown_fields() {
    let json = r#"{"openai":"sk-x","unknown_field":"value","custom_endpoints":[]}"#;
    let keys: ApiKeys = serde_json::from_str(json).unwrap();
    assert_eq!(keys.openai, Some("sk-x".into()));
    assert!(keys.custom_endpoints.is_empty());
}

#[test]
fn default_profile_settings_fall_back_to_legacy_fields_before_migration() {
    let keys = ApiKeys {
        openai: Some("sk-openai".into()),
        anthropic: Some("sk-anthropic".into()),
        openai_base_url: Some("http://127.0.0.1:1234/v1".into()),
        custom_endpoints: vec![endpoint("ep", "https://a.io", "key", &[("m", None)])],
        ..Default::default()
    };

    let settings = keys.profile_settings(DEFAULT_PROFILE_INFERENCE_KEY);
    assert_eq!(settings.openai.as_deref(), Some("sk-openai"));
    assert_eq!(settings.anthropic.as_deref(), Some("sk-anthropic"));
    assert_eq!(
        settings.openai_base_url.as_deref(),
        Some("http://127.0.0.1:1234/v1")
    );
    assert_eq!(settings.custom_endpoints.len(), 1);
}

#[test]
fn local_settings_migration_populates_default_profile_once() {
    let mut keys = ApiKeys {
        openai: Some("sk-openai".into()),
        ..Default::default()
    };

    assert!(keys.migrate_default_profile_if_needed());
    assert!(keys.migrate_default_profile_local_settings_if_needed(
        Some(" http://127.0.0.1:1234/v1/ ".into()),
        r#"{"auto-autocomplete":"local/model"}"#.into(),
        "local/model,local/other".into(),
        true,
    ));

    let settings = keys.profile_settings(DEFAULT_PROFILE_INFERENCE_KEY);
    assert_eq!(settings.openai.as_deref(), Some("sk-openai"));
    assert_eq!(
        settings.openai_base_url.as_deref(),
        Some("http://127.0.0.1:1234/v1")
    );
    assert_eq!(
        settings.local_model_aliases,
        r#"{"auto-autocomplete":"local/model"}"#
    );
    assert_eq!(settings.local_model_list, "local/model,local/other");
    assert!(settings.local_ai_autocomplete_enabled);

    assert!(!keys.migrate_default_profile_local_settings_if_needed(
        Some("http://different.test/v1".into()),
        r#"{"auto":"different"}"#.into(),
        "different".into(),
        false,
    ));
    let settings = keys.profile_settings(DEFAULT_PROFILE_INFERENCE_KEY);
    assert_eq!(
        settings.openai_base_url.as_deref(),
        Some("http://127.0.0.1:1234/v1")
    );
    assert!(settings.local_ai_autocomplete_enabled);
}

// ── has_any_key ─────────────────────────────────────────────────

#[test]
fn has_any_key_false_when_empty() {
    assert!(!ApiKeys::default().has_any_key());
}

#[test]
fn has_any_key_true_for_openai_only() {
    let keys = ApiKeys {
        openai: Some("sk-x".into()),
        ..Default::default()
    };
    assert!(keys.has_any_key());
}

#[test]
fn has_any_key_true_for_custom_endpoints_only() {
    let keys = ApiKeys {
        custom_endpoints: vec![endpoint("ep", "https://a.io", "key", &[("m", None)])],
        ..Default::default()
    };
    assert!(keys.has_any_key());
}

#[test]
fn has_any_key_false_for_endpoint_with_empty_api_key() {
    let keys = ApiKeys {
        custom_endpoints: vec![endpoint("ep", "https://a.io", "", &[("m", None)])],
        ..Default::default()
    };
    assert!(!keys.has_any_key());
}

// ── has_custom_endpoints

#[test]
fn has_custom_endpoints_false_when_empty() {
    assert!(!ApiKeys::default().has_custom_endpoints());
}

#[test]
fn has_custom_endpoints_true_when_present() {
    let keys = ApiKeys {
        custom_endpoints: vec![endpoint("ep", "https://a.io", "k", &[("m", None)])],
        ..Default::default()
    };
    assert!(keys.has_custom_endpoints());
}

// ── custom_model_providers_for_request ──────────────────────────

#[test]
fn custom_model_providers_none_when_empty() {
    let mgr = make_manager(ApiKeys::default());
    assert!(mgr
        .custom_model_providers_for_request(DEFAULT_PROFILE_INFERENCE_KEY, true)
        .is_none());
}

#[test]
fn custom_model_providers_none_when_byo_disabled() {
    let mgr = make_manager(ApiKeys {
        custom_endpoints: vec![endpoint("ep", "https://a.io", "k", &[("m", None)])],
        ..Default::default()
    });
    assert!(mgr
        .custom_model_providers_for_request(DEFAULT_PROFILE_INFERENCE_KEY, false)
        .is_none());
}

#[test]
fn custom_model_providers_populates_single_endpoint() {
    let mgr = make_manager(ApiKeys {
        custom_endpoints: vec![endpoint_with_keys(
            "My EP",
            "https://custom.io/v1",
            "ep-key",
            &[("big-model", Some("alias"), "uuid-1")],
        )],
        ..Default::default()
    });
    let result = mgr
        .custom_model_providers_for_request(DEFAULT_PROFILE_INFERENCE_KEY, true)
        .unwrap();
    assert_eq!(result.providers.len(), 1);
    let p = &result.providers[0];
    assert_eq!(p.base_url, "https://custom.io/v1");
    assert_eq!(p.api_key, "ep-key");
    assert_eq!(p.models.len(), 1);
    assert_eq!(p.models[0].slug, "big-model");
    assert_eq!(p.models[0].config_key, "uuid-1");
}

#[test]
fn multiple_endpoints_all_serialize() {
    let mgr = make_manager(ApiKeys {
        custom_endpoints: vec![
            endpoint_with_keys(
                "ep1",
                "https://a.io",
                "k1",
                &[("gpt-4", Some("fast"), "uuid-a")],
            ),
            endpoint_with_keys(
                "ep2",
                "https://b.io",
                "k2",
                &[
                    ("llama-70b", None, "uuid-b"),
                    ("mixtral", Some("mix"), "uuid-c"),
                ],
            ),
        ],
        ..Default::default()
    });
    let result = mgr
        .custom_model_providers_for_request(DEFAULT_PROFILE_INFERENCE_KEY, true)
        .unwrap();
    assert_eq!(result.providers.len(), 2);
    assert_eq!(result.providers[0].base_url, "https://a.io");
    assert_eq!(result.providers[0].models[0].config_key, "uuid-a");
    assert_eq!(result.providers[1].base_url, "https://b.io");
    assert_eq!(result.providers[1].models.len(), 2);
    assert_eq!(result.providers[1].models[0].slug, "llama-70b");
    assert_eq!(result.providers[1].models[0].config_key, "uuid-b");
    assert_eq!(result.providers[1].models[1].config_key, "uuid-c");
}

#[test]
fn byok_disabled_returns_none_even_with_endpoints() {
    let mgr = make_manager(ApiKeys {
        custom_endpoints: vec![endpoint("ep", "https://a.io", "k", &[("m", None)])],
        ..Default::default()
    });
    assert!(mgr
        .custom_model_providers_for_request(DEFAULT_PROFILE_INFERENCE_KEY, false)
        .is_none());
}

#[test]
fn empty_api_key_endpoints_are_skipped() {
    let mgr = make_manager(ApiKeys {
        custom_endpoints: vec![
            endpoint_with_keys("empty", "https://a.io", "", &[("m", None, "uuid-x")]),
            endpoint_with_keys("ok", "https://b.io", "k", &[("m", None, "uuid-y")]),
        ],
        ..Default::default()
    });
    let result = mgr
        .custom_model_providers_for_request(DEFAULT_PROFILE_INFERENCE_KEY, true)
        .unwrap();
    assert_eq!(result.providers.len(), 1);
    assert_eq!(result.providers[0].base_url, "https://b.io");
}

#[test]
fn endpoints_with_only_empty_models_are_skipped() {
    let mgr = make_manager(ApiKeys {
        custom_endpoints: vec![endpoint_with_keys(
            "ep",
            "https://a.io",
            "k",
            &[("", None, "uuid-z")],
        )],
        ..Default::default()
    });
    assert!(mgr
        .custom_model_providers_for_request(DEFAULT_PROFILE_INFERENCE_KEY, true)
        .is_none());
}

// ── display_label fallback ─────────────────────────────────────

#[test]
fn display_label_uses_alias_when_present() {
    let m = CustomEndpointModel {
        name: "raw-name".into(),
        alias: Some("My Alias".into()),
        config_key: "k".into(),
    };
    assert_eq!(m.display_label(), "My Alias");
}

#[test]
fn display_label_falls_back_to_name_when_alias_missing() {
    let m = CustomEndpointModel {
        name: "raw-name".into(),
        alias: None,
        config_key: "k".into(),
    };
    assert_eq!(m.display_label(), "raw-name");
}

#[test]
fn display_label_falls_back_to_name_when_alias_is_whitespace() {
    let m = CustomEndpointModel {
        name: "raw-name".into(),
        alias: Some("   ".into()),
        config_key: "k".into(),
    };
    assert_eq!(m.display_label(), "raw-name");
}

// ── api_keys_for_request ────────────────────────────────────────

#[test]
fn api_keys_for_request_none_when_empty() {
    let mgr = make_manager(ApiKeys::default());
    assert!(mgr
        .api_keys_for_request(DEFAULT_PROFILE_INFERENCE_KEY, true, false)
        .is_none());
}

#[test]
fn api_keys_for_request_populates_provider_keys() {
    let mgr = make_manager(ApiKeys {
        openai: Some("sk-o".into()),
        anthropic: Some("sk-a".into()),
        ..Default::default()
    });
    let result = mgr
        .api_keys_for_request(DEFAULT_PROFILE_INFERENCE_KEY, true, false)
        .unwrap();
    assert_eq!(result.openai, "sk-o");
    assert_eq!(result.anthropic, "sk-a");
    assert!(result.google.is_empty());
}

#[test]
fn api_keys_for_request_omits_keys_when_byo_disabled() {
    let mgr = make_manager(ApiKeys {
        openai: Some("sk-o".into()),
        ..Default::default()
    });
    // With BYO disabled and no other credentials, returns None.
    assert!(mgr
        .api_keys_for_request(DEFAULT_PROFILE_INFERENCE_KEY, false, false)
        .is_none());
}

#[test]
fn api_keys_for_request_none_for_custom_endpoints_only() {
    let mgr = make_manager(ApiKeys {
        custom_endpoints: vec![endpoint("ep", "https://a.io", "k", &[("m", None)])],
        ..Default::default()
    });
    assert!(mgr
        .api_keys_for_request(DEFAULT_PROFILE_INFERENCE_KEY, true, false)
        .is_none());
}

#[test]
fn api_keys_for_request_uses_requested_profile() {
    let mut keys = ApiKeys::default();
    keys.profile_inference_settings.insert(
        DEFAULT_PROFILE_INFERENCE_KEY.to_string(),
        ProfileInferenceSettings {
            openai: Some("sk-default".into()),
            ..Default::default()
        },
    );
    keys.profile_inference_settings.insert(
        "profile-2".to_string(),
        ProfileInferenceSettings {
            openai: Some("sk-profile-2".into()),
            anthropic: Some("sk-anthropic-2".into()),
            ..Default::default()
        },
    );
    let mgr = make_manager(keys);

    let result = mgr
        .api_keys_for_request("profile-2", true, false)
        .expect("profile scoped keys should be present");
    assert_eq!(result.openai, "sk-profile-2");
    assert_eq!(result.anthropic, "sk-anthropic-2");
}

#[test]
fn custom_model_providers_for_request_uses_requested_profile() {
    let mut keys = ApiKeys::default();
    keys.profile_inference_settings.insert(
        DEFAULT_PROFILE_INFERENCE_KEY.to_string(),
        ProfileInferenceSettings {
            custom_endpoints: vec![endpoint_with_keys(
                "default",
                "https://default.test/v1",
                "default-key",
                &[("default-model", None, "default-cfg")],
            )],
            ..Default::default()
        },
    );
    keys.profile_inference_settings.insert(
        "profile-2".to_string(),
        ProfileInferenceSettings {
            custom_endpoints: vec![endpoint_with_keys(
                "profile",
                "https://profile.test/v1",
                "profile-key",
                &[("profile-model", None, "profile-cfg")],
            )],
            ..Default::default()
        },
    );
    let mgr = make_manager(keys);

    let result = mgr
        .custom_model_providers_for_request("profile-2", true)
        .expect("profile custom endpoint should be present");
    assert_eq!(result.providers.len(), 1);
    assert_eq!(result.providers[0].base_url, "https://profile.test/v1");
    assert_eq!(result.providers[0].api_key, "profile-key");
    assert_eq!(result.providers[0].models[0].slug, "profile-model");
    assert_eq!(result.providers[0].models[0].config_key, "profile-cfg");
}
