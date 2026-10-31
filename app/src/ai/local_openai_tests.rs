use super::*;

#[test]
fn normalizes_chat_completion_urls() {
    assert_eq!(
        chat_completions_url("http://127.0.0.1:11434/v1"),
        "http://127.0.0.1:11434/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("http://127.0.0.1:11434"),
        "http://127.0.0.1:11434/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("http://127.0.0.1:11434/v1/chat/completions"),
        "http://127.0.0.1:11434/v1/chat/completions"
    );
}

#[test]
fn normalizes_responses_urls() {
    for base in [
        "http://127.0.0.1:11434",
        "http://127.0.0.1:11434/v1",
        "http://127.0.0.1:11434/v1/",
        "http://127.0.0.1:11434/v1/responses",
        "http://127.0.0.1:11434/v1/chat/completions",
        "http://127.0.0.1:11434/v1/chat/completions/",
    ] {
        assert_eq!(
            responses_url(base),
            "http://127.0.0.1:11434/v1/responses",
            "base url: {base}"
        );
    }
    // A non-`/v1` prefix is preserved when rewriting the chat endpoint.
    assert_eq!(
        responses_url("https://example.test/api/chat/completions"),
        "https://example.test/api/responses"
    );
}

#[test]
fn parses_fenced_json_content() {
    let parsed: PredictionJson =
        parse_json_content("```json\n{\"suggestion\":\"git status\"}\n```").unwrap();
    assert_eq!(parsed.into_string(), "git status");
}

#[cfg(not(target_family = "wasm"))]
#[test]
fn flattens_chat_messages_into_one_prompt() {
    let messages = vec![
        ChatMessage {
            role: "system",
            content: "Reply with one shell command.\n".to_string(),
        },
        ChatMessage {
            role: "user",
            content: "list files".to_string(),
        },
    ];
    assert_eq!(
        flatten_messages(&messages),
        "System:\nReply with one shell command.\n\nUser:\nlist files"
    );
}

fn client(
    provider_kind: LocalProviderKind,
    base_url: &str,
    opencode_command: &str,
) -> LocalOpenAIClient {
    LocalOpenAIClient::new(LocalOpenAISettingsSnapshot {
        enabled: true,
        base_url: base_url.to_string(),
        command_model: "model".to_string(),
        prediction_model: "model".to_string(),
        timeout_ms: 1_000,
        provider_kind,
        opencode_command: opencode_command.to_string(),
        ..Default::default()
    })
}

#[test]
fn opencode_needs_its_command_not_a_base_url() {
    let runnable = client(LocalProviderKind::OpenCode, "", "opencode");
    assert!(runnable.configured_for_command().is_ok());
    assert!(runnable.configured_for_prediction().is_ok());

    let missing_binary = client(LocalProviderKind::OpenCode, "http://127.0.0.1:1/v1", "  ");
    assert!(matches!(
        missing_binary.configured_for_command(),
        Err(LocalOpenAIError::NotConfigured)
    ));
    assert!(matches!(
        missing_binary.configured_for_prediction(),
        Err(LocalOpenAIError::NotConfigured)
    ));
}

#[test]
fn openai_compatible_needs_a_base_url_not_the_opencode_command() {
    let runnable = client(
        LocalProviderKind::OpenAICompatible,
        "http://127.0.0.1:1234/v1",
        "",
    );
    assert!(runnable.configured_for_command().is_ok());
    assert!(runnable.configured_for_prediction().is_ok());

    let missing_url = client(LocalProviderKind::OpenAICompatible, " ", "opencode");
    assert!(matches!(
        missing_url.configured_for_command(),
        Err(LocalOpenAIError::NotConfigured)
    ));
    assert!(matches!(
        missing_url.configured_for_prediction(),
        Err(LocalOpenAIError::NotConfigured)
    ));
}

#[test]
fn responses_endpoint_is_openai_compatible_only() {
    let compatible = LocalOpenAISettingsSnapshot {
        base_url: "http://127.0.0.1:1234/v1".to_string(),
        api_key: "key".to_string(),
        ..Default::default()
    };
    assert_eq!(
        LocalOpenAIClient::responses_endpoint(&compatible).unwrap(),
        ("http://127.0.0.1:1234/v1".to_string(), "key".to_string())
    );

    let opencode = LocalOpenAISettingsSnapshot {
        provider_kind: LocalProviderKind::OpenCode,
        ..Default::default()
    };
    assert!(matches!(
        LocalOpenAIClient::responses_endpoint(&opencode),
        Err(LocalOpenAIError::ResponsesShape(_))
    ));
}

/// A request seen by [`spawn_mock_server`].
struct RecordedRequest {
    path: String,
    body: serde_json::Value,
}

/// Serves one canned JSON response per connection, in order, on a loopback port, and records the
/// requests it receives. Connections past the end of `responses` get a 500.
fn spawn_mock_server(responses: Vec<serde_json::Value>) -> (u16, Arc<Mutex<Vec<RecordedRequest>>>) {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let recorded_for_thread = Arc::clone(&recorded);
    std::thread::spawn(move || {
        let mut responses = responses.into_iter();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };

            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head_len, content_length) = loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break (raw.len(), 0);
                }
                raw.extend_from_slice(&chunk[..n]);
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (end + 4, length);
                }
            };
            while raw.len() < head_len + content_length {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..n]);
            }

            let head = String::from_utf8_lossy(&raw[..head_len.min(raw.len())]).into_owned();
            let path = head
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or_default()
                .to_owned();
            let body = serde_json::from_slice(&raw[head_len.min(raw.len())..])
                .unwrap_or(serde_json::Value::Null);
            recorded_for_thread
                .lock()
                .push(RecordedRequest { path, body });

            let (status, payload) = match responses.next() {
                Some(payload) => ("200 OK", payload.to_string()),
                None => ("500 Internal Server Error", "{}".to_owned()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
        }
    });
    (port, recorded)
}

fn test_http_client() -> http_client::Client {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    http_client::Client::new_for_test()
}

fn responses_client(port: u16, use_responses_api: bool) -> LocalOpenAIClient {
    LocalOpenAIClient::new(LocalOpenAISettingsSnapshot {
        enabled: true,
        base_url: format!("http://127.0.0.1:{port}/v1"),
        command_model: "model".to_string(),
        prediction_model: "model".to_string(),
        timeout_ms: 5_000,
        use_responses_api,
        ..Default::default()
    })
}

fn responses_reply(text: &str, reasoning_ids: &[&str]) -> serde_json::Value {
    let mut output: Vec<serde_json::Value> = reasoning_ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "type": "reasoning",
                "id": id,
                "encrypted_content": format!("enc-{id}"),
            })
        })
        .collect();
    output.push(serde_json::json!({
        "type": "message",
        "content": [{ "type": "output_text", "text": text }],
    }));
    serde_json::json!({ "output": output })
}

fn user_messages() -> Vec<ChatMessage> {
    vec![
        ChatMessage {
            role: "system",
            content: "Answer briefly.".to_string(),
        },
        ChatMessage {
            role: "user",
            content: "hello".to_string(),
        },
    ]
}

#[tokio::test(flavor = "current_thread")]
async fn single_shot_requests_use_chat_completions_when_the_flag_is_off() {
    let (port, requests) = spawn_mock_server(vec![serde_json::json!({
        "choices": [{ "message": { "content": "from chat" } }],
    })]);
    let client = responses_client(port, false);
    let config = client.configured_for_command().unwrap();

    let text = client
        .chat_completion(
            &test_http_client(),
            &config,
            "model".into(),
            user_messages(),
        )
        .await
        .unwrap();

    assert_eq!(text, "from chat");
    let requests = requests.lock();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/chat/completions");
    assert_eq!(requests[0].body["messages"][1]["content"], "hello");
}

#[tokio::test(flavor = "current_thread")]
async fn single_shot_requests_use_the_responses_api_when_the_flag_is_on() {
    let (port, requests) = spawn_mock_server(vec![responses_reply("from responses", &["rs_1"])]);
    let client = responses_client(port, true);
    let config = client.configured_for_command().unwrap();

    let text = client
        .chat_completion(
            &test_http_client(),
            &config,
            "model".into(),
            user_messages(),
        )
        .await
        .unwrap();

    assert_eq!(text, "from responses");
    let requests = requests.lock();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/responses");
    let input = requests[0].body["input"].as_array().unwrap();
    assert_eq!(input.len(), 2);
    assert_eq!(input[0]["role"], "system");
    assert_eq!(input[1]["role"], "user");
    assert_eq!(input[1]["content"], "hello");
    // Single-shot callers have no conversation, so nothing is carried over.
    assert_eq!(client.reasoning_cache.lock().conversation_count(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn responses_conversations_send_back_only_the_previous_turns_reasoning() {
    let (port, requests) = spawn_mock_server(vec![
        responses_reply("one", &["rs_1"]),
        responses_reply("two", &["rs_2", "rs_3"]),
        responses_reply("three", &[]),
        responses_reply("four", &["rs_4"]),
    ]);
    let client = responses_client(port, true);
    let http = test_http_client();
    let ask = |text: &'static str| {
        client.chat_completion_responses(&http, "conv", text.to_string(), None)
    };
    let reasoning_ids = |requests: &[RecordedRequest], index: usize| -> Vec<String> {
        requests[index].body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "reasoning")
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    };

    assert_eq!(ask("a").await.unwrap(), "one");
    assert_eq!(ask("b").await.unwrap(), "two");
    assert_eq!(ask("c").await.unwrap(), "three");
    assert_eq!(ask("d").await.unwrap(), "four");

    let requests = requests.lock();
    assert!(requests.iter().all(|r| r.path == "/v1/responses"));
    // Turn 1 has nothing to send back.
    assert!(reasoning_ids(&requests, 0).is_empty());
    // Turn 2 sends turn 1's item, before the new user message.
    assert_eq!(reasoning_ids(&requests, 1), ["rs_1"]);
    let turn_2 = requests[1].body["input"].as_array().unwrap();
    assert_eq!(turn_2.last().unwrap()["content"], "b");
    // Turn 3 sends only turn 2's items: turn 1's are not accumulated.
    assert_eq!(reasoning_ids(&requests, 2), ["rs_2", "rs_3"]);
    // Turn 3 returned no reasoning, which ends the carryover for turn 4.
    assert!(reasoning_ids(&requests, 3).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn clearing_a_conversation_drops_its_reasoning_state() {
    let (port, requests) = spawn_mock_server(vec![
        responses_reply("one", &["rs_1"]),
        responses_reply("two", &[]),
    ]);
    let client = responses_client(port, true);
    let http = test_http_client();

    client
        .chat_completion_responses(&http, "conv", "a".to_string(), None)
        .await
        .unwrap();
    assert_eq!(client.reasoning_cache.lock().conversation_count(), 1);

    client.clear_reasoning_state("conv");
    assert_eq!(client.reasoning_cache.lock().conversation_count(), 0);

    client
        .chat_completion_responses(&http, "conv", "b".to_string(), None)
        .await
        .unwrap();
    let requests = requests.lock();
    assert!(
        requests[1].body["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["type"] != "reasoning"),
        "state cleared before turn 2 must not be sent"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_conversation_api_requires_the_flag() {
    let client = responses_client(1, false);
    let err = client
        .chat_completion_responses(&test_http_client(), "conv", "a".to_string(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, LocalOpenAIError::ResponsesShape(_)));
}
