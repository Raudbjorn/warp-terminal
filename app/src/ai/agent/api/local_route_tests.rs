use warp_multi_agent_api as api;

use super::*;

fn custom_providers() -> api::request::settings::CustomModelProviders {
    api::request::settings::CustomModelProviders {
        providers: vec![
            api::request::settings::custom_model_providers::CustomModelProvider {
                base_url: "https://llm.example.com/v1".to_string(),
                api_key: "sk-test".to_string(),
                schema: api::request::settings::custom_model_providers::CustomEndpointSchema::OpenaiChatCompletions as i32,
                models: vec![
                    api::request::settings::custom_model_providers::CustomModel {
                        slug: "llama-3.3-70b".to_string(),
                        config_key: "cfg-123".to_string(),
                        reasoning_effort: String::new(),
                    },
                ],
            },
        ],
    }
}

fn user_query_input() -> api::request::input::Type {
    api::request::input::Type::UserInputs(api::request::input::UserInputs {
        inputs: vec![api::request::input::user_inputs::UserInput {
            input: Some(
                api::request::input::user_inputs::user_input::Input::UserQuery(
                    api::request::input::UserQuery {
                        query: "hi".to_string(),
                        ..Default::default()
                    },
                ),
            ),
        }],
    })
}

fn request_for_model(
    model: &str,
    providers: Option<api::request::settings::CustomModelProviders>,
    input_type: api::request::input::Type,
) -> api::Request {
    api::Request {
        task_context: Some(api::request::TaskContext { tasks: vec![] }),
        input: Some(api::request::Input {
            context: None,
            r#type: Some(input_type),
        }),
        settings: Some(api::request::Settings {
            model_config: Some(api::request::settings::ModelConfig {
                base: model.to_string(),
                ..Default::default()
            }),
            custom_model_providers: providers,
            ..Default::default()
        }),
        metadata: Some(api::request::Metadata::default()),
        existing_suggestions: None,
        mcp_context: None,
    }
}

#[test]
fn routes_custom_endpoint_model_to_local_loop() {
    let request = request_for_model("cfg-123", Some(custom_providers()), user_query_input());

    let route = resolve_local_route(&request).expect("custom endpoint model should route locally");

    assert_eq!(route.endpoint.base_url, "https://llm.example.com/v1");
    assert_eq!(route.endpoint.api_key, "sk-test");
    assert_eq!(route.endpoint.model_slug, "llama-3.3-70b");
    assert_eq!(route.config_key, "cfg-123");
}

#[test]
fn does_not_route_models_without_a_custom_provider() {
    let request = request_for_model(
        "claude-4-sonnet",
        Some(custom_providers()),
        user_query_input(),
    );

    assert!(resolve_local_route(&request).is_none());
}

#[test]
fn does_not_route_when_no_providers_are_configured() {
    let request = request_for_model("cfg-123", None, user_query_input());

    assert!(resolve_local_route(&request).is_none());
}

#[test]
fn routes_passive_suggestion_requests_locally() {
    let request = request_for_model(
        "cfg-123",
        Some(custom_providers()),
        api::request::input::Type::GeneratePassiveSuggestions(
            api::request::input::GeneratePassiveSuggestions::default(),
        ),
    );

    let route = resolve_local_route(&request)
        .expect("passive suggestion requests for custom endpoint models should route locally");
    assert_eq!(route.config_key, "cfg-123");
}

#[test]
fn does_not_route_system_driven_inputs_like_code_review() {
    let request = request_for_model(
        "cfg-123",
        Some(custom_providers()),
        api::request::input::Type::CodeReview(api::request::input::CodeReview::default()),
    );

    assert!(resolve_local_route(&request).is_none());
}

// ── route_request: the hard guard against leaking to the server ──────────

fn accepted_passive_suggestion_input() -> api::request::input::Type {
    use api::request::input::user_inputs::user_input::Input;
    use api::request::input::user_inputs::{PassiveSuggestionResultInput, UserInput};
    api::request::input::Type::UserInputs(api::request::input::UserInputs {
        inputs: vec![
            UserInput {
                input: Some(Input::PassiveSuggestionResult(
                    PassiveSuggestionResultInput {
                        result: Some(api::PassiveSuggestionResultType {
                            trigger: Some(
                                api::passive_suggestion_result_type::Trigger::ExecutedShellCommand(
                                    api::ExecutedShellCommand {
                                        command: "fd . .go".to_string(),
                                        exit_code: 1,
                                        ..Default::default()
                                    },
                                ),
                            ),
                            suggestion: Some(
                                api::passive_suggestion_result_type::Suggestion::Prompt(
                                    api::passive_suggestion_result_type::Prompt {
                                        prompt: "Find go files".to_string(),
                                    },
                                ),
                            ),
                        }),
                    },
                )),
            },
            UserInput {
                input: Some(Input::UserQuery(api::request::input::UserQuery {
                    query: "Find go files".to_string(),
                    ..Default::default()
                })),
            },
        ],
    })
}

#[test]
fn route_request_serves_accepted_passive_suggestion_locally() {
    // Accepting a passive suggestion submits a batch of
    // [PassiveSuggestionResult, UserQuery]; it must route locally, not be
    // blocked as an unsupported input.
    let request = request_for_model(
        "cfg-123",
        Some(custom_providers()),
        accepted_passive_suggestion_input(),
    );

    assert!(matches!(
        route_request(&request),
        RequestDisposition::Local(_)
    ));
}

#[test]
fn route_request_serves_custom_endpoint_model_locally() {
    let request = request_for_model("cfg-123", Some(custom_providers()), user_query_input());

    assert!(matches!(
        route_request(&request),
        RequestDisposition::Local(_)
    ));
}

#[test]
fn route_request_blocks_builtin_model_from_server() {
    // A built-in model can't be served locally, and it must NOT fall through to
    // the Warp server.
    let request = request_for_model(
        "claude-4-sonnet",
        Some(custom_providers()),
        user_query_input(),
    );

    assert!(matches!(
        route_request(&request),
        RequestDisposition::BlockedFromServer(_)
    ));
}

#[test]
fn route_request_blocks_unsupported_input_from_server() {
    // Even for a custom endpoint model, an input the local engine cannot serve
    // must not reach the server.
    let request = request_for_model(
        "cfg-123",
        Some(custom_providers()),
        api::request::input::Type::CodeReview(api::request::input::CodeReview::default()),
    );

    assert!(matches!(
        route_request(&request),
        RequestDisposition::BlockedFromServer(_)
    ));
}

fn request_with_schema(schema: i32) -> api::Request {
    let mut providers = custom_providers();
    providers.providers[0].schema = schema;
    request_for_model("cfg-123", Some(providers), user_query_input())
}

fn block_reason_of(request: &api::Request) -> &'static str {
    match route_request(request) {
        RequestDisposition::BlockedFromServer(reason) => reason,
        other => panic!("expected the request to be blocked, got {other:?}"),
    }
}

#[test]
fn routes_chat_completions_providers_to_the_chat_schema() {
    use api::request::settings::custom_model_providers::CustomEndpointSchema;

    let request = request_with_schema(CustomEndpointSchema::OpenaiChatCompletions as i32);

    let route = resolve_local_route(&request).expect("chat completions should route locally");
    assert_eq!(
        route.endpoint.schema,
        local_agent::EndpointSchema::ChatCompletions
    );
}

#[test]
fn routes_responses_providers_to_the_responses_schema() {
    use api::request::settings::custom_model_providers::CustomEndpointSchema;

    let request = request_with_schema(CustomEndpointSchema::OpenaiResponses as i32);

    let route = resolve_local_route(&request).expect("responses should route locally");
    assert_eq!(
        route.endpoint.schema,
        local_agent::EndpointSchema::Responses
    );
    assert_eq!(route.endpoint.base_url, "https://llm.example.com/v1");
    assert_eq!(route.endpoint.model_slug, "llama-3.3-70b");
    assert_eq!(route.config_key, "cfg-123");
    assert!(matches!(
        route_request(&request),
        RequestDisposition::Local(_)
    ));
}

#[test]
fn schema_follows_the_provider_that_owns_the_selected_model() {
    use api::request::settings::custom_model_providers::{
        CustomEndpointSchema, CustomModel, CustomModelProvider,
    };

    let mut providers = custom_providers();
    providers.providers.push(CustomModelProvider {
        base_url: "https://api.openai.example/v1".to_string(),
        api_key: "sk-responses".to_string(),
        schema: CustomEndpointSchema::OpenaiResponses as i32,
        models: vec![CustomModel {
            slug: "gpt-5".to_string(),
            config_key: "cfg-456".to_string(),
            reasoning_effort: String::new(),
        }],
    });

    let chat = request_for_model("cfg-123", Some(providers.clone()), user_query_input());
    let responses = request_for_model("cfg-456", Some(providers), user_query_input());

    assert_eq!(
        resolve_local_route(&chat)
            .expect("chat route")
            .endpoint
            .schema,
        local_agent::EndpointSchema::ChatCompletions
    );
    assert_eq!(
        resolve_local_route(&responses)
            .expect("responses route")
            .endpoint
            .schema,
        local_agent::EndpointSchema::Responses
    );
}

#[test]
fn anthropic_messages_providers_stay_blocked_from_server_with_an_accurate_reason() {
    use api::request::settings::custom_model_providers::CustomEndpointSchema;

    let request = request_with_schema(CustomEndpointSchema::AnthropicMessages as i32);

    assert!(resolve_local_route(&request).is_none());
    let reason = block_reason_of(&request);
    assert!(
        reason.contains("Anthropic Messages") && reason.contains("OpenAI Responses"),
        "reason should name the unsupported schema and the supported ones: {reason}"
    );
}

#[test]
fn unrecognized_endpoint_schemas_stay_blocked_from_server() {
    let request = request_with_schema(i32::MAX);

    assert!(resolve_local_route(&request).is_none());
    assert!(block_reason_of(&request).contains("does not recognize"));
}

#[test]
fn block_reason_distinguishes_unsupported_input_unknown_model_and_schema() {
    use api::request::settings::custom_model_providers::CustomEndpointSchema;

    let unsupported_input = request_for_model(
        "cfg-123",
        Some(custom_providers()),
        api::request::input::Type::CodeReview(api::request::input::CodeReview::default()),
    );
    let not_custom = request_for_model(
        "claude-4-sonnet",
        Some(custom_providers()),
        user_query_input(),
    );
    let anthropic = request_with_schema(CustomEndpointSchema::AnthropicMessages as i32);

    let reasons = [
        block_reason_of(&unsupported_input),
        block_reason_of(&not_custom),
        block_reason_of(&anthropic),
    ];
    assert!(reasons[0].contains("kind of request"));
    assert!(reasons[1].contains("not a custom endpoint model"));
    assert!(reasons[2].contains("Anthropic Messages"));
}
