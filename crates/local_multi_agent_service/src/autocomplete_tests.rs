use super::*;

#[test]
fn parses_plain_prefix_matching_command() {
    assert_eq!(
        parse_autocomplete_command("git checkout feature/local-autocomplete", "git checkout "),
        Some("git checkout feature/local-autocomplete".to_string())
    );
}

#[test]
fn parses_json_command_candidate() {
    assert_eq!(
        parse_autocomplete_command(
            r#"{"commands":["cargo test -p local_multi_agent_service"]}"#,
            "cargo test",
        ),
        Some("cargo test -p local_multi_agent_service".to_string())
    );
}

#[test]
fn parses_fenced_single_line_command() {
    assert_eq!(
        parse_autocomplete_command("```sh\nls crates/local_multi_agent_service\n```", "ls "),
        Some("ls crates/local_multi_agent_service".to_string())
    );
}

#[test]
fn rejects_explanations_and_non_prefix_commands() {
    assert_eq!(
        parse_autocomplete_command("You can run `git status`.", "git "),
        None
    );
    assert_eq!(
        parse_autocomplete_command("cargo check -p app", "git "),
        None
    );
}

#[test]
fn rejects_multiline_candidates() {
    assert_eq!(
        parse_autocomplete_command("git status\ngit diff", "git "),
        None
    );
}

#[test]
fn parses_harmony_final_command() {
    assert_eq!(
        parse_autocomplete_command(
            "<|channel>thought\nNeed autocomplete.\n<|channel>final\ngit checkout feature/local",
            "git chec",
        ),
        Some("git checkout feature/local".to_string())
    );
}

#[test]
fn rejects_harmony_thought_without_final_command() {
    assert_eq!(
        parse_autocomplete_command("<|channel>thought\nNeed autocomplete.", "git "),
        None
    );
}

#[test]
fn response_falls_back_to_matching_history_when_model_output_is_invalid() {
    let request = LocalCommandAutocompleteRequest {
        prefix: "cargo test --".to_string(),
        history: vec![
            "cargo test --doc".to_string(),
            "cargo test --workspace".to_string(),
        ],
        ..Default::default()
    };

    let response = LocalCommandAutocompleteResponse::from_raw_output(
        "<|channel>thought\nNeed autocomplete.".to_string(),
        &request,
    );

    assert_eq!(response.most_likely_action, "cargo test --doc");
}

#[test]
fn response_falls_back_to_package_candidate_for_package_argument() {
    let request = LocalCommandAutocompleteRequest {
        prefix: "cargo test -p ".to_string(),
        file_candidates: vec![
            "Cargo.toml".to_string(),
            "app".to_string(),
            "crates/local_multi_agent_service".to_string(),
        ],
        ..Default::default()
    };

    let response = LocalCommandAutocompleteResponse::from_raw_output(
        "<|channel>thought\nNeed autocomplete.".to_string(),
        &request,
    );

    assert_eq!(
        response.most_likely_action,
        "cargo test -p local_multi_agent_service"
    );
}

#[test]
fn response_falls_back_to_file_candidate_for_argument_position() {
    let request = LocalCommandAutocompleteRequest {
        prefix: "sed -n '1,120p' ".to_string(),
        file_candidates: vec![
            "crates/local_multi_agent_service/src/provider.rs".to_string(),
            "README.md".to_string(),
        ],
        ..Default::default()
    };

    let response = LocalCommandAutocompleteResponse::from_raw_output(
        "<|channel>thought\nNeed autocomplete.".to_string(),
        &request,
    );

    assert_eq!(
        response.most_likely_action,
        "sed -n '1,120p' crates/local_multi_agent_service/src/provider.rs"
    );
}

#[test]
fn response_falls_back_to_docker_container_id_from_recent_ps_output() {
    let request = LocalCommandAutocompleteRequest {
        prefix: "docker logs -f 5".to_string(),
        recent_blocks: vec![AutocompleteBlockContext {
            command: "docker ps".to_string(),
            output: Some(
                "CONTAINER ID   IMAGE                            COMMAND                   CREATED        STATUS                             PORTS                                                                                      NAMES\n\
5a8038634ee7   rhasspy/wyoming-whisper          \"bash docker_run.sh ...\"    6 weeks ago    Up 4 minutes                       0.0.0.0:10300->10300/tcp, [::]:10300->10300/tcp                                            wyoming-whisper\n\
1c3fa9b2abf3   openmemory-openmemory            \"npm start\"               5 months ago   Restarting (1) 29 seconds ago                                                                                                 openmemory-openmemory-1"
                    .to_string(),
            ),
            ..Default::default()
        }],
        ..Default::default()
    };

    let response = LocalCommandAutocompleteResponse::from_raw_output(
        "<|channel>thought\nNeed autocomplete.".to_string(),
        &request,
    );

    assert_eq!(response.most_likely_action, "docker logs -f 5a8038634ee7");
}

#[test]
fn provider_messages_include_prompt_and_context() {
    let request = LocalCommandAutocompleteRequest {
        prefix: "git check".to_string(),
        cwd: Some("/repo".to_string()),
        shell: Some("zsh".to_string()),
        platform: Some("macOS".to_string()),
        history: vec!["git status".to_string()],
        file_candidates: vec!["Cargo.toml".to_string()],
        ..Default::default()
    };

    let messages = autocomplete_provider_messages(&request);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
    let user_content = messages[1].content.as_ref().unwrap().to_string();
    assert!(user_content.contains("git check"));
    assert!(user_content.contains("Cargo.toml"));
}
