use serde_json::{Value, json};

use crate::{
    config::Config,
    provider::{LocalModelConfig, ProviderRuntime},
    store::{
        GenericStringObjectInput, GenericStringObjectRecord, IntegrationConfigPatch,
        IntegrationRecord, IntegrationStore,
    },
};

#[derive(Debug, Clone)]
pub struct GraphqlResult {
    pub status: u16,
    pub payload: Value,
    pub diagnostics: GraphqlDiagnostics,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphqlDiagnostics {
    pub operation_name: Option<String>,
    pub query_operation_name: Option<String>,
    pub op_from_query_string: Option<String>,
    pub canonical_operation_name: Option<String>,
    pub variable_keys: Vec<String>,
    pub input_keys: Vec<String>,
}

pub async fn handle_local_graphql_request(
    request: Value,
    store: &mut IntegrationStore,
    provider: &ProviderRuntime,
    config: &Config,
    op_from_query_string: Option<&str>,
) -> GraphqlResult {
    let operation_name = infer_operation_name(&request, op_from_query_string);
    let canonical = operation_name.as_deref().map(canonical_operation_name);
    let diagnostics = graphql_diagnostics(
        &request,
        op_from_query_string,
        operation_name.clone(),
        canonical.clone(),
    );

    let result = match canonical.as_deref() {
        Some("createSimpleIntegration") => create_simple_integration(store, variables_of(&request)),
        Some("simpleIntegrations") => simple_integrations(store, variables_of(&request)),
        Some("getOAuthConnectTxStatus") => Ok(get_oauth_connect_tx_status()),
        Some("getIntegrationsUsingEnvironment") => {
            get_integrations_using_environment(store, variables_of(&request))
        }
        Some("userGithubInfo") => Ok(user_github_info()),
        Some("userRepoAuthStatus") => user_repo_auth_status(variables_of(&request)),
        Some("suggestCloudEnvironmentImage") => Ok(suggest_cloud_environment_image()),
        Some("createGenericStringObject") => {
            create_generic_string_object(store, variables_of(&request))
        }
        Some("updateGenericStringObject") => {
            update_generic_string_object(store, variables_of(&request))
        }
        Some("bulkCreateObjects") => bulk_create_objects(store, variables_of(&request)),
        Some("updatedCloudObjects") => get_updated_cloud_objects(store),
        Some("getCloudEnvironments") => Ok(get_cloud_environments()),
        Some("featureModelChoice") => Ok(get_feature_model_choices(provider, config).await),
        Some("freeAvailableModels") => Ok(free_available_models(provider, config).await),
        Some("getUser") => Ok(get_user(provider, config).await),
        Some("getUserSettings") => Ok(get_user_settings()),
        Some("updateUserSettings") => Ok(update_user_settings()),
        Some("listAIConversations") => Ok(list_ai_conversations()),
        Some("getRequestLimitInfo") => Ok(get_request_limit_info()),
        Some("getReferralInfo") => Ok(get_referral_info()),
        Some("getConversationUsage") => Ok(get_conversation_usage()),
        Some("workspacesMetadataForUser") => Ok(get_workspaces_metadata_for_user()),
        _ => return unsupported_operation(operation_name, diagnostics),
    };

    match result {
        Ok(payload) => GraphqlResult {
            status: 200,
            payload,
            diagnostics,
        },
        Err(error) => GraphqlResult {
            status: 400,
            payload: json!({
                "data": null,
                "errors": [{ "message": error.to_string() }],
            }),
            diagnostics,
        },
    }
}

pub fn graphql_error_messages(payload: &Value) -> Vec<String> {
    payload
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string())
        })
        .collect()
}

fn graphql_diagnostics(
    request: &Value,
    op_from_query_string: Option<&str>,
    operation_name: Option<String>,
    canonical_operation_name: Option<String>,
) -> GraphqlDiagnostics {
    let variables = variables_of(request);
    let input = input_of(variables);
    GraphqlDiagnostics {
        operation_name,
        query_operation_name: query_operation_name(request).map(str::to_owned),
        op_from_query_string: op_from_query_string.and_then(non_empty).map(str::to_owned),
        canonical_operation_name,
        variable_keys: sorted_keys(variables),
        input_keys: sorted_keys(input),
    }
}

fn infer_operation_name(request: &Value, op_from_query_string: Option<&str>) -> Option<String> {
    request
        .get("operationName")
        .and_then(Value::as_str)
        .and_then(non_empty)
        .or_else(|| op_from_query_string.and_then(non_empty))
        .map(str::to_owned)
        .or_else(|| {
            let query = request.get("query")?.as_str()?;
            [
                "createSimpleIntegration",
                "simpleIntegrations",
                "createGenericStringObject",
                "updateGenericStringObject",
                "bulkCreateObjects",
                "getOAuthConnectTxStatus",
                "getIntegrationsUsingEnvironment",
                "userGithubInfo",
                "userRepoAuthStatus",
                "suggestCloudEnvironmentImage",
                "getUpdatedCloudObjects",
                "updatedCloudObjects",
                "getCloudEnvironments",
                "getRequestLimitInfo",
                "getReferralInfo",
                "getFeatureModelChoices",
                "featureModelChoice",
                "freeAvailableModels",
                "getUserSettings",
                "listAIConversations",
                "conversationUsage",
                "getConversationUsage",
                "getUser",
                "getWorkspacesMetadataForUser",
                "workspacesMetadataForUser",
                "updateUserSettings",
                "pricingInfo",
            ]
            .into_iter()
            .find(|candidate| query.contains(candidate))
            .map(str::to_owned)
        })
}

fn query_operation_name(request: &Value) -> Option<&str> {
    let query = request.get("query")?.as_str()?;
    let trimmed = query.trim_start();
    for prefix in ["query", "mutation"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest
                .trim_start()
                .split(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
                .find(|value| !value.is_empty());
        }
    }
    None
}

fn canonical_operation_name(name: &str) -> String {
    match name {
        "CreateSimpleIntegration" | "createSimpleIntegration" => "createSimpleIntegration",
        "SimpleIntegrations" | "simpleIntegrations" => "simpleIntegrations",
        "get_oauth_connect_tx_status" | "GetOAuthConnectTxStatus" | "getOAuthConnectTxStatus" => {
            "getOAuthConnectTxStatus"
        }
        "GetIntegrationsUsingEnvironment" | "getIntegrationsUsingEnvironment" => {
            "getIntegrationsUsingEnvironment"
        }
        "user_github_info" | "UserGithubInfo" | "userGithubInfo" => "userGithubInfo",
        "user_repo_auth_status" | "UserRepoAuthStatus" | "userRepoAuthStatus" => {
            "userRepoAuthStatus"
        }
        "suggest_cloud_environment_image"
        | "SuggestCloudEnvironmentImage"
        | "suggestCloudEnvironmentImage" => "suggestCloudEnvironmentImage",
        "GetUpdatedCloudObjects" | "getUpdatedCloudObjects" | "updatedCloudObjects" => {
            "updatedCloudObjects"
        }
        "GetCloudEnvironmentsQuery" | "GetCloudEnvironments" | "getCloudEnvironments" => {
            "getCloudEnvironments"
        }
        "CreateGenericStringObject" | "createGenericStringObject" => "createGenericStringObject",
        "UpdateGenericStringObject" | "updateGenericStringObject" => "updateGenericStringObject",
        "BulkCreateObjects" | "bulkCreateObjects" => "bulkCreateObjects",
        "GetFeatureModelChoices" | "getFeatureModelChoices" | "featureModelChoice" => {
            "featureModelChoice"
        }
        "FreeAvailableModels" | "free_available_models" | "freeAvailableModels" => {
            "freeAvailableModels"
        }
        "GetUser" | "getUser" => "getUser",
        "GetUserSettings" | "getUserSettings" => "getUserSettings",
        "UpdateUserSettings" | "updateUserSettings" => "updateUserSettings",
        "ListAIConversationMetadata" | "ListAIConversations" | "listAIConversations" => {
            "listAIConversations"
        }
        "GetRequestLimitInfo" | "getRequestLimitInfo" => "getRequestLimitInfo",
        "GetReferralInfo" | "getReferralInfo" => "getReferralInfo",
        "GetConversationUsage" | "getConversationUsage" | "conversationUsage" => {
            "getConversationUsage"
        }
        "GetWorkspacesMetadataForUser"
        | "getWorkspacesMetadataForUser"
        | "workspacesMetadataForUser"
        | "pricingInfo" => "workspacesMetadataForUser",
        other => other,
    }
    .to_owned()
}

fn create_simple_integration(
    store: &mut IntegrationStore,
    variables: &Value,
) -> anyhow::Result<Value> {
    let input = input_of(variables);
    let integration_type = required_string(
        value_at(variables, &["integrationType", "integration_type"])
            .or_else(|| value_at(input, &["integrationType", "integration_type"])),
        "integrationType",
    )?;
    let enabled = optional_bool(
        value_at(variables, &["enabled"]).or_else(|| value_at(input, &["enabled"])),
        true,
    )?;
    let is_update = optional_bool(
        value_at(variables, &["isUpdate", "is_update"])
            .or_else(|| value_at(input, &["isUpdate", "is_update"])),
        false,
    )?;
    let record = store.create_or_update(
        integration_type,
        enabled,
        integration_config_from_variables(variables)?,
        is_update,
    )?;
    Ok(json!({
        "data": {
            "createSimpleIntegration": {
                "__typename": "CreateSimpleIntegrationOutput",
                "authUrl": null,
                "success": true,
                "message": format!("Local {} integration saved.", record.provider_slug),
                "txId": null,
            }
        }
    }))
}

fn simple_integrations(store: &mut IntegrationStore, variables: &Value) -> anyhow::Result<Value> {
    let input = input_of(variables);
    let providers = value_at(input, &["providers"])
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("input.providers is required"))?
        .iter()
        .map(|value| required_string(Some(value), "provider").map(str::to_owned))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let integrations = store
        .list(&providers)?
        .into_iter()
        .map(|(provider_slug, record)| simple_integration_payload(&provider_slug, record.as_ref()))
        .collect::<Vec<_>>();
    Ok(json!({
        "data": {
            "simpleIntegrations": {
                "__typename": "SimpleIntegrationsOutput",
                "integrations": integrations,
                "message": null,
            }
        }
    }))
}

fn get_oauth_connect_tx_status() -> Value {
    json!({
        "data": {
            "getOAuthConnectTxStatus": {
                "__typename": "GetOAuthConnectTxStatusOutput",
                "status": "COMPLETED",
            }
        }
    })
}

fn get_integrations_using_environment(
    store: &mut IntegrationStore,
    variables: &Value,
) -> anyhow::Result<Value> {
    let environment_id = required_string(
        value_at(input_of(variables), &["environmentId", "environment_id"]),
        "environmentId",
    )?;
    Ok(json!({
        "data": {
            "getIntegrationsUsingEnvironment": {
                "__typename": "GetIntegrationsUsingEnvironmentOutput",
                "providerNames": store.providers_using_environment(environment_id)?,
            }
        }
    }))
}

fn user_github_info() -> Value {
    json!({
        "data": {
            "userGithubInfo": {
                "__typename": "GithubConnectedOutput",
                "username": "local",
                "installedRepos": [],
                "appInstallLink": "",
            }
        }
    })
}

fn user_repo_auth_status(variables: &Value) -> anyhow::Result<Value> {
    let repos = value_at(input_of(variables), &["repos"])
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("input.repos is required"))?;
    let statuses = repos
        .iter()
        .map(|repo| {
            Ok(json!({
                "owner": required_string(value_at(repo, &["owner"]), "repo.owner")?,
                "repo": required_string(value_at(repo, &["repo"]), "repo.repo")?,
                "status": "SUCCESS",
                "isPublic": true,
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(json!({
        "data": {
            "userRepoAuthStatus": {
                "__typename": "UserRepoAuthStatusOutput",
                "statuses": statuses,
                "authUrl": null,
                "txId": null,
            }
        }
    }))
}

fn suggest_cloud_environment_image() -> Value {
    json!({
        "data": {
            "suggestCloudEnvironmentImage": {
                "__typename": "SuggestCloudEnvironmentImageOutput",
                "detectedLanguages": [],
                "image": "ubuntu:24.04",
                "needsCustomImage": false,
                "reason": "Local no-cloud mode uses a deterministic default image.",
                "responseContext": response_context(),
            }
        }
    })
}

fn create_generic_string_object(
    store: &mut IntegrationStore,
    variables: &Value,
) -> anyhow::Result<Value> {
    let record = store.create_generic_string_object(generic_string_object_input_from_value(
        value_at(
            input_of(variables),
            &["genericStringObject", "generic_string_object"],
        )
        .ok_or_else(|| anyhow::anyhow!("input.genericStringObject is required"))?,
    )?)?;
    Ok(json!({
        "data": {
            "createGenericStringObject": create_generic_string_object_output(&record),
        }
    }))
}

fn bulk_create_objects(store: &mut IntegrationStore, variables: &Value) -> anyhow::Result<Value> {
    let objects = value_at(
        value_at(
            input_of(variables),
            &["genericStringObjects", "generic_string_objects"],
        )
        .unwrap_or(&Value::Null),
        &["objects"],
    )
    .and_then(Value::as_array)
    .ok_or_else(|| anyhow::anyhow!("input.genericStringObjects.objects is required"))?;
    let inputs = objects
        .iter()
        .map(generic_string_object_input_from_value)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let records = store.bulk_create_generic_string_objects(&inputs)?;
    Ok(json!({
        "data": {
            "bulkCreateObjects": {
                "__typename": "BulkCreateObjectsOutput",
                "genericStringObjects": {
                    "__typename": "BulkCreateGenericStringObjectsOutput",
                    "objects": records.iter().map(create_generic_string_object_output).collect::<Vec<_>>(),
                },
                "responseContext": response_context(),
            }
        }
    }))
}

fn update_generic_string_object(
    store: &mut IntegrationStore,
    variables: &Value,
) -> anyhow::Result<Value> {
    let input = input_of(variables);
    let uid = required_string(value_at(input, &["uid"]), "uid")?;
    let serialized_model = required_string(
        value_at(input, &["serializedModel", "serialized_model"]),
        "serializedModel",
    )?;
    let record = store.update_generic_string_object(uid, serialized_model)?;
    Ok(json!({
        "data": {
            "updateGenericStringObject": {
                "__typename": "UpdateGenericStringObjectOutput",
                "responseContext": response_context(),
                "update": {
                    "__typename": "ObjectUpdateSuccess",
                    "lastEditorUid": "local-user",
                    "revisionTs": record.revision_ts,
                }
            }
        }
    }))
}

fn get_updated_cloud_objects(store: &mut IntegrationStore) -> anyhow::Result<Value> {
    Ok(json!({
        "data": {
            "updatedCloudObjects": {
                "__typename": "UpdatedCloudObjectsOutput",
                "actionHistories": [],
                "deletedObjectUids": {
                    "folderUids": [],
                    "genericStringObjectUids": [],
                    "notebookUids": [],
                    "workflowUids": [],
                },
                "folders": [],
                "genericStringObjects": store.list_generic_string_objects()?.iter().map(generic_string_object_payload).collect::<Vec<_>>(),
                "mcpGallery": [],
                "notebooks": [],
                "responseContext": response_context(),
                "userProfiles": [],
                "workflows": [],
            }
        }
    }))
}

fn get_cloud_environments() -> Value {
    json!({
        "data": {
            "getCloudEnvironments": {
                "__typename": "GetCloudEnvironmentsOutput",
                "cloudEnvironments": [],
                "responseContext": response_context(),
            }
        }
    })
}

async fn get_feature_model_choices(provider: &ProviderRuntime, config: &Config) -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": {
                    "workspaces": [{
                        "featureModelChoice": local_feature_model_choice(provider, config).await,
                    }]
                }
            }
        }
    })
}

async fn free_available_models(provider: &ProviderRuntime, config: &Config) -> Value {
    json!({
        "data": {
            "freeAvailableModels": {
                "__typename": "FreeAvailableModelsOutput",
                "featureModelChoice": local_feature_model_choice(provider, config).await,
                "responseContext": response_context(),
            }
        }
    })
}

async fn get_user(provider: &ProviderRuntime, config: &Config) -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "apiKeyOwnerType": null,
                "principalType": "USER",
                "user": {
                    "anonymousUserInfo": null,
                    "experiments": [],
                    "isOnWorkDomain": false,
                    "isOnboarded": true,
                    "profile": {
                        "displayName": "Local User",
                        "email": "local@warp.dev",
                        "needsSsoLink": false,
                        "photoUrl": null,
                        "uid": "local-user",
                    },
                    "llms": local_feature_model_choice(provider, config).await,
                }
            }
        }
    })
}

fn get_user_settings() -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": {
                    "settings": {
                        "isCloudConversationStorageEnabled": false,
                        "isCrashReportingEnabled": false,
                        "isTelemetryEnabled": false,
                    }
                }
            }
        }
    })
}

fn get_referral_info() -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": {
                    "referrals": {
                        "referralCode": "",
                        "numberClaimed": 0,
                        "isReferred": false,
                    }
                }
            }
        }
    })
}

fn get_request_limit_info() -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": {
                    "workspaces": [],
                    "requestLimitInfo": {
                        "isUnlimited": true,
                        "requestsUsedSinceLastRefresh": 0,
                        "requestLimit": 1_000_000,
                        "nextRefreshTime": "2999-01-01T00:00:00.000Z",
                        "requestLimitRefreshDuration": "MONTHLY",
                        "isUnlimitedVoice": true,
                        "voiceRequestLimit": 1_000_000,
                        "voiceRequestsUsedSinceLastRefresh": 0,
                        "isUnlimitedCodebaseIndices": true,
                        "maxCodebaseIndices": 1_000_000,
                        "maxFilesPerRepo": 1_000_000,
                        "embeddingGenerationBatchSize": 100,
                    },
                    "bonusGrants": [],
                }
            }
        }
    })
}

fn update_user_settings() -> Value {
    json!({
        "data": {
            "updateUserSettings": {
                "__typename": "UpdateUserSettingsOutput",
                "responseContext": response_context(),
            }
        }
    })
}

fn list_ai_conversations() -> Value {
    json!({
        "data": {
            "listAIConversations": {
                "__typename": "ListAIConversationsOutput",
                "conversations": [],
                "responseContext": response_context(),
            }
        }
    })
}

fn get_conversation_usage() -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": {
                    "conversationUsage": [],
                }
            }
        }
    })
}

fn get_workspaces_metadata_for_user() -> Value {
    json!({
        "data": {
            "user": {
                "__typename": "UserOutput",
                "user": {
                    "workspaces": [],
                    "experiments": [],
                    "discoverableTeams": [],
                }
            },
            "pricingInfo": {
                "__typename": "PricingInfoOutput",
                "pricingInfo": {
                    "plans": [],
                    "overages": {
                        "pricePerRequestUsdCents": 0,
                    },
                    "addonCreditsOptions": [],
                }
            }
        }
    })
}

async fn local_feature_model_choice(provider: &ProviderRuntime, config: &Config) -> Value {
    let available = local_available_llms(provider, config).await;
    json!({
        "agentMode": available,
        "planning": available,
        "coding": available,
        "cliAgent": available,
        "computerUseAgent": available,
    })
}

async fn local_available_llms(provider: &ProviderRuntime, config: &Config) -> Value {
    let models = provider.fetch_provider_models(config).await;
    let default_id = config
        .openai_model
        .as_deref()
        .filter(|configured| models.iter().any(|model| model.id == *configured))
        .unwrap_or_else(|| {
            models
                .first()
                .map(|model| model.id.as_str())
                .unwrap_or(crate::config::DEFAULT_MODEL)
        });
    json!({
        "defaultId": default_id,
        "choices": models.iter().map(local_model_info).collect::<Vec<_>>(),
        "preferredCodexModelId": null,
    })
}

fn local_model_info(model: &LocalModelConfig) -> Value {
    json!({
        "displayName": model.display_name,
        "baseModelName": model.base_model_name,
        "id": model.id,
        "reasoningLevel": model.reasoning_level,
        "usageMetadata": {
            "requestMultiplier": model.request_multiplier.max(1.0),
            "creditMultiplier": model.credit_multiplier,
        },
        "description": model.description,
        "disableReason": normalize_disable_reason(model.disable_reason.as_deref()),
        "visionSupported": model.vision_supported,
        "spec": null,
        "provider": model.provider,
        "hostConfigs": [{
            "enabled": true,
            "modelRoutingHost": "DIRECT_API",
        }],
        "pricing": {
            "discountPercentage": null,
        }
    })
}

fn integration_config_from_variables(variables: &Value) -> anyhow::Result<IntegrationConfigPatch> {
    let input = input_of(variables);
    let config = value_at(variables, &["config"])
        .or_else(|| value_at(input, &["config"]))
        .unwrap_or(&Value::Null);
    Ok(IntegrationConfigPatch {
        base_prompt: optional_string_patch(value_at(config, &["basePrompt", "base_prompt"]))?,
        environment_uid: optional_string_patch(value_at(
            config,
            &["environmentUid", "environment_uid"],
        ))?,
        mcp_servers_json: optional_string_patch(value_at(
            config,
            &["mcpServersJson", "mcp_servers_json"],
        ))?,
        model_id: optional_string_patch(value_at(config, &["modelId", "model_id"]))?,
        remove_mcp_server_names: optional_string_array_patch(value_at(
            config,
            &["removeMcpServerNames", "remove_mcp_server_names"],
        ))?,
        worker_host: optional_string_patch(value_at(config, &["workerHost", "worker_host"]))?,
    })
}

fn simple_integration_payload(provider_slug: &str, record: Option<&IntegrationRecord>) -> Value {
    let description = match provider_slug {
        "linear" => "Connect Linear to local Warp agents.",
        "slack" => "Connect Slack to local Warp agents.",
        _ => {
            return simple_integration_payload_with_description(
                provider_slug,
                &format!("Local {provider_slug} integration."),
                record,
            );
        }
    };
    simple_integration_payload_with_description(provider_slug, description, record)
}

fn simple_integration_payload_with_description(
    provider_slug: &str,
    description: &str,
    record: Option<&IntegrationRecord>,
) -> Value {
    if let Some(record) = record {
        json!({
            "providerSlug": provider_slug,
            "description": description,
            "connectionStatus": if record.enabled { "ACTIVE" } else { "NOT_ENABLED" },
            "integrationConfig": {
                "environmentUid": record.environment_uid.as_deref().unwrap_or(""),
                "basePrompt": record.base_prompt.as_deref().unwrap_or(""),
                "modelId": record.model_id.as_deref().unwrap_or(""),
                "mcpServersJson": record.mcp_servers_json,
            },
            "createdAt": record.created_at,
            "updatedAt": record.updated_at,
        })
    } else {
        json!({
            "providerSlug": provider_slug,
            "description": description,
            "connectionStatus": "INTEGRATION_NOT_CONFIGURED",
            "integrationConfig": null,
            "createdAt": null,
            "updatedAt": null,
        })
    }
}

fn generic_string_object_input_from_value(
    value: &Value,
) -> anyhow::Result<GenericStringObjectInput> {
    Ok(GenericStringObjectInput {
        client_id: optional_string(value_at(value, &["clientId", "client_id"]))?,
        format: required_string(value_at(value, &["format"]), "genericStringObject.format")?
            .to_owned(),
        serialized_model: required_string(
            value_at(value, &["serializedModel", "serialized_model"]),
            "genericStringObject.serializedModel",
        )?
        .to_owned(),
    })
}

fn create_generic_string_object_output(record: &GenericStringObjectRecord) -> Value {
    json!({
        "__typename": "CreateGenericStringObjectOutput",
        "clientId": record.client_id.as_deref().unwrap_or(&record.uid),
        "genericStringObject": generic_string_object_payload(record),
        "responseContext": response_context(),
        "revisionTs": record.revision_ts,
    })
}

fn generic_string_object_payload(record: &GenericStringObjectRecord) -> Value {
    json!({
        "__typename": "GenericStringObject",
        "format": record.format,
        "metadata": object_metadata(record),
        "permissions": object_permissions(record),
        "serializedModel": record.serialized_model,
    })
}

fn object_metadata(record: &GenericStringObjectRecord) -> Value {
    json!({
        "__typename": "ObjectMetadata",
        "creatorUid": "local-user",
        "currentEditorUid": null,
        "isWelcomeObject": false,
        "lastEditorUid": "local-user",
        "metadataLastUpdatedTs": record.metadata_last_updated_ts,
        "parent": {
            "__typename": "Space",
            "type": "User",
            "uid": "local-user",
        },
        "revisionTs": record.revision_ts,
        "trashedTs": null,
        "uid": record.uid,
    })
}

fn object_permissions(record: &GenericStringObjectRecord) -> Value {
    json!({
        "__typename": "ObjectPermissions",
        "anyoneLinkSharing": null,
        "guests": [],
        "lastUpdatedTs": record.permissions_last_updated_ts,
        "space": {
            "__typename": "Space",
            "type": "User",
            "uid": "local-user",
        },
    })
}

fn unsupported_operation(
    operation_name: Option<String>,
    diagnostics: GraphqlDiagnostics,
) -> GraphqlResult {
    GraphqlResult {
        status: 400,
        payload: json!({
            "data": null,
            "errors": [{
                "message": format!("unsupported_operation: {}", operation_name.as_deref().unwrap_or("unknown")),
            }],
        }),
        diagnostics,
    }
}

fn response_context() -> Value {
    json!({ "serverVersion": "local" })
}

fn normalize_disable_reason(reason: Option<&str>) -> Option<&str> {
    let reason = reason?;
    matches!(
        reason,
        "AdminDisabled" | "OutOfRequests" | "ProviderOutage" | "RequiresUpgrade"
    )
    .then_some(reason)
}

fn variables_of(request: &Value) -> &Value {
    request
        .get("variables")
        .filter(|value| value.is_object())
        .unwrap_or(&Value::Null)
}

fn input_of(variables: &Value) -> &Value {
    variables
        .get("input")
        .filter(|value| value.is_object())
        .unwrap_or(&Value::Null)
}

fn value_at<'a>(source: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let object = source.as_object()?;
    keys.iter().find_map(|key| object.get(*key))
}

fn required_string<'a>(value: Option<&'a Value>, name: &str) -> anyhow::Result<&'a str> {
    value
        .and_then(Value::as_str)
        .and_then(non_empty)
        .ok_or_else(|| anyhow::anyhow!("{name} is required"))
}

fn optional_string(value: Option<&Value>) -> anyhow::Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(non_empty(value).map(str::to_owned)),
        _ => anyhow::bail!("expected string value"),
    }
}

fn optional_string_patch(value: Option<&Value>) -> anyhow::Result<Option<Option<String>>> {
    match value {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(value)) => Ok(Some(non_empty(value).map(str::to_owned))),
        _ => anyhow::bail!("expected string value"),
    }
}

fn optional_string_array_patch(value: Option<&Value>) -> anyhow::Result<Option<Vec<String>>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| required_string(Some(value), "string array item").map(str::to_owned))
            .collect::<anyhow::Result<Vec<_>>>()
            .map(Some),
        _ => anyhow::bail!("expected string array value"),
    }
}

fn optional_bool(value: Option<&Value>, fallback: bool) -> anyhow::Result<bool> {
    match value {
        None | Some(Value::Null) => Ok(fallback),
        Some(Value::Bool(value)) => Ok(*value),
        _ => anyhow::bail!("expected boolean value"),
    }
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys = value
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    keys.sort();
    keys
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_operation_from_query_body() {
        let request = json!({ "query": "query Whatever { freeAvailableModels { __typename } }" });
        assert_eq!(
            infer_operation_name(&request, None).as_deref(),
            Some("freeAvailableModels")
        );
    }
}
