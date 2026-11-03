//! Unit tests for the pure parts of the local next-command adapter: transcript
//! building, model-output parsing, and custom-endpoint resolution.

use ai::api_keys::{CustomEndpoint, CustomEndpointModel, CustomEndpointSchema};

use super::*;
fn resolved(route: LocalInputRoute) -> LocalEndpointConfig {
    match route {
        LocalInputRoute::Custom(endpoint) => endpoint,
        other => panic!("expected a custom endpoint, got {other:?}"),
    }
}

fn request_with_prefix(prefix: Option<&str>) -> GenerateAIInputSuggestionsRequest {
    GenerateAIInputSuggestionsRequest {
        context_messages: vec![r#"{"input":"fd . .go","output":"[fd error]"}"#.to_string()],
        history_context: "git status\ngit commit".to_string(),
        system_context: Some(r#"{"os":"macos","shell":"zsh"}"#.to_string()),
        rejected_suggestions: vec!["rm -rf /".to_string()],
        prefix: prefix.map(str::to_string),
        block_context: None,
        previous_result: None,
    }
}

// ── transcript ──────────────────────────────────────────────────────────

#[test]
fn transcript_is_system_then_user_with_context() {
    let transcript = build_suggestion_transcript(&request_with_prefix(None));

    assert_eq!(transcript.len(), 2);
    let ChatMessage::System(system) = &transcript[0] else {
        panic!("first message must be the system prompt");
    };
    assert!(system.contains("next shell command"));
    assert!(system.contains("macos"), "system context must be embedded");

    let ChatMessage::User(user) = &transcript[1] else {
        panic!("second message must be the user message");
    };
    assert!(user.contains("fd . .go"), "recent blocks must be present");
    assert!(user.contains("git status"), "history must be present");
    assert!(
        user.contains("rm -rf /"),
        "rejected suggestions must be listed"
    );
}

#[test]
fn transcript_instructs_prefix_completion_when_present() {
    let transcript = build_suggestion_transcript(&request_with_prefix(Some("fd -")));
    let ChatMessage::User(user) = &transcript[1] else {
        panic!("user message");
    };
    assert!(
        user.contains("fd -"),
        "the prefix must be included so the model completes it"
    );
}

// ── parsing ─────────────────────────────────────────────────────────────

#[test]
fn parses_a_bare_command() {
    let response = parse_command_suggestion("fd -e go .", None);
    assert_eq!(response.commands, vec!["fd -e go .".to_string()]);
    assert_eq!(response.most_likely_action, "fd -e go .");
}

#[test]
fn strips_code_fences_and_backticks() {
    assert_eq!(
        parse_command_suggestion("```sh\nfd -e go .\n```", None).most_likely_action,
        "fd -e go ."
    );
    assert_eq!(
        parse_command_suggestion("`fd -e go .`", None).most_likely_action,
        "fd -e go ."
    );
    assert_eq!(
        parse_command_suggestion("```fd -e go .```", None).most_likely_action,
        "fd -e go ."
    );
}

#[test]
fn takes_the_first_real_line() {
    assert_eq!(
        parse_command_suggestion("\n\nfd -e go .\nsome trailing prose", None).most_likely_action,
        "fd -e go ."
    );
}

#[test]
fn empty_output_yields_no_suggestion() {
    let response = parse_command_suggestion("   \n  \n", None);
    assert!(response.commands.is_empty());
    assert!(response.most_likely_action.is_empty());
}

#[test]
fn prefix_mismatch_is_dropped() {
    // The model ignored the prefix; we must not offer a command that doesn't
    // extend what the user already typed.
    let response = parse_command_suggestion("ls -la", Some("fd "));
    assert!(response.commands.is_empty());
}

#[test]
fn prefix_match_is_kept() {
    let response = parse_command_suggestion("fd -e go .", Some("fd "));
    assert_eq!(response.most_likely_action, "fd -e go .");
}

// ── endpoint resolution ─────────────────────────────────────────────────

fn endpoint(name: &str, url: &str, key: &str, models: &[(&str, &str)]) -> CustomEndpoint {
    CustomEndpoint {
        name: name.to_string(),
        url: url.to_string(),
        api_key: key.to_string(),
        schema: Default::default(),
        models: models
            .iter()
            .map(|(model_name, config_key)| CustomEndpointModel {
                name: model_name.to_string(),
                alias: None,
                config_key: config_key.to_string(),
            })
            .collect(),
    }
}

#[test]
fn picks_endpoint_by_active_model_config_key() {
    let endpoints = vec![endpoint(
        "Ollama",
        "http://localhost:11434/v1",
        "ollama",
        &[("qwen3:8b", "cfg-123")],
    )];

    let resolved = resolved(pick_custom_endpoint("cfg-123", &endpoints));
    assert_eq!(resolved.base_url, "http://localhost:11434/v1");
    assert_eq!(resolved.api_key, "ollama");
    assert_eq!(resolved.model_slug, "qwen3:8b");
}

#[test]
fn returns_none_for_builtin_model() {
    let endpoints = vec![endpoint(
        "Ollama",
        "http://localhost:11434/v1",
        "ollama",
        &[("qwen3:8b", "cfg-123")],
    )];
    assert!(matches!(
        pick_custom_endpoint("claude-4-sonnet", &endpoints),
        LocalInputRoute::Server
    ));
}

#[test]
fn skips_endpoints_with_empty_url() {
    let endpoints = vec![
        endpoint("NoUrl", "  ", "k", &[("m", "cfg-1")]),
        endpoint(
            "WithEmptyKey",
            "http://localhost:11434/v1",
            "",
            &[("m", "cfg-2")],
        ),
    ];
    assert!(matches!(
        pick_custom_endpoint("cfg-1", &endpoints),
        LocalInputRoute::Server
    ));
    assert!(matches!(
        pick_custom_endpoint("cfg-2", &endpoints),
        LocalInputRoute::Custom(_)
    ));
}

fn endpoint_with_schema(schema: CustomEndpointSchema, config_key: &str) -> CustomEndpoint {
    CustomEndpoint {
        schema,
        ..endpoint(
            "Provider",
            "https://llm.example.com/v1",
            "sk-test",
            &[("gpt-5", config_key)],
        )
    }
}

#[test]
fn chat_completions_endpoints_resolve_to_the_chat_schema() {
    let endpoints = vec![endpoint_with_schema(
        CustomEndpointSchema::OpenaiChatCompletions,
        "cfg-1",
    )];

    let resolved = resolved(pick_custom_endpoint("cfg-1", &endpoints));
    assert_eq!(resolved.schema, EndpointSchema::ChatCompletions);
}

#[test]
fn responses_endpoints_resolve_to_the_responses_schema() {
    let endpoints = vec![endpoint_with_schema(
        CustomEndpointSchema::OpenaiResponses,
        "cfg-1",
    )];

    let resolved = resolved(pick_custom_endpoint("cfg-1", &endpoints));
    assert_eq!(resolved.schema, EndpointSchema::Responses);
    assert_eq!(resolved.model_slug, "gpt-5");
}

#[test]
fn schema_follows_the_matched_endpoint_not_the_first_one() {
    let endpoints = vec![
        endpoint_with_schema(CustomEndpointSchema::OpenaiChatCompletions, "cfg-chat"),
        endpoint_with_schema(CustomEndpointSchema::OpenaiResponses, "cfg-responses"),
    ];

    let resolved = resolved(pick_custom_endpoint("cfg-responses", &endpoints));
    assert_eq!(resolved.schema, EndpointSchema::Responses);
}

#[test]
fn anthropic_messages_endpoints_are_reported_as_unsupported() {
    let endpoints = vec![endpoint_with_schema(
        CustomEndpointSchema::AnthropicMessages,
        "cfg-1",
    )];

    assert!(matches!(
        pick_custom_endpoint("cfg-1", &endpoints),
        LocalInputRoute::UnsupportedSchema(_)
    ));
}
