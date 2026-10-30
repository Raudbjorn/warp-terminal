use std::{collections::HashMap, convert::Infallible, net::SocketAddr, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use prost::Message as _;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};
use warp_multi_agent_api as api;

use crate::{
    autocomplete::LocalCommandAutocompleteRequest,
    config::{Config, LogLevel, SERVICE_VERSION, non_empty_str},
    graphql::{graphql_error_messages, handle_local_graphql_request},
    logger::Logger,
    provider::{
        ChatCompletionParams, FinishReason, LocalAgentError, ProviderChatMessage, ProviderResponse,
        ProviderRuntime, assistant_message, content_with_images, messages_from_stored_conversation,
        provider_messages_to_json, system_message, tool_result_message, user_text_message,
    },
    request::{SuggestPlanStatus, WarpRequestSummary, WarpToolResult, decode_warp_request},
    response,
    store::IntegrationStore,
};

const MAX_REQUEST_BYTES: usize = 25 * 1024 * 1024;
const OPENAI_BASE_URL_HEADER: &str = "x-warp-openai-base-url";
const LOCAL_MODEL_ALIASES_HEADER: &str = "x-warp-local-model-aliases";

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    logger: Logger,
    store: Arc<Mutex<IntegrationStore>>,
    conversations: Arc<Mutex<HashMap<String, ConversationState>>>,
    provider: ProviderRuntime,
}

#[derive(Clone, Default)]
struct ConversationState {
    messages: Vec<ProviderChatMessage>,
}

pub async fn run(config: Config) -> Result<()> {
    let logger = Logger::new(config.log_level, config.local_service_log_path.clone());
    let mut store = IntegrationStore::open(&config.local_graphql_db_path)?;
    let conversations =
        load_conversation_state(&mut store, config.local_max_history_messages, &logger).await;
    let state = AppState {
        config: Arc::new(config),
        logger,
        store: Arc::new(Mutex::new(store)),
        conversations: Arc::new(Mutex::new(conversations)),
        provider: ProviderRuntime::new(),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/session/{session_id}", get(shared_session_redirect))
        .route("/ai/multi-agent", post(multi_agent))
        .route("/ai/passive-suggestions", post(passive_suggestions))
        .route(
            "/ai/local-command-autocomplete",
            post(local_command_autocomplete),
        )
        .route("/graphql/v2", post(graphql))
        .with_state(state.clone());

    let addr: SocketAddr = format!("{}:{}", state.config.host, state.config.port)
        .parse()
        .with_context(|| {
            format!(
                "invalid local service bind address {}",
                state.config.root_url()
            )
        })?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {}", state.config.root_url()))?;
    state
        .logger
        .log(
            LogLevel::Info,
            "server_started",
            json!({
                "url": state.config.root_url(),
                "logLevel": state.config.log_level.as_str(),
                "logFilePath": state.config.local_service_log_path,
                "hasOpenAiBaseUrlEnv": state.config.openai_base_url.as_ref().and_then(|value| non_empty_str(value)).is_some(),
                "hasOpenAiModelEnv": state.config.openai_model.as_ref().and_then(|value| non_empty_str(value)).is_some(),
                "hasModelAliasesEnv": state.config.local_model_aliases.as_ref().and_then(|value| non_empty_str(value)).is_some(),
                "localGraphqlDbPath": state.config.local_graphql_db_path,
                "conversationStateCount": state.conversations.lock().await.len(),
                "maxConversationMessages": state.config.local_max_history_messages,
            }),
        )
        .await;
    axum::serve(listener, app)
        .await
        .context("local multi-agent service failed")
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    state
        .logger
        .log(
            LogLevel::Info,
            "http_request",
            json!({ "method": "GET", "path": "/health" }),
        )
        .await;
    (
        StatusCode::OK,
        axum::Json(json!({
            "ok": true,
            "version": SERVICE_VERSION,
            "configHash": state.config.local_config_hash,
        })),
    )
}

async fn shared_session_redirect(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    uri: Uri,
) -> impl IntoResponse {
    let mut location = format!(
        "{}://shared_session/{session_id}",
        state.config.warp_url_scheme
    );
    if let Some(query) = uri.query() {
        location.push('?');
        location.push_str(query);
    }
    let escaped = escape_html(&location);
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(format!(
            r#"<!doctype html><html><head><meta http-equiv="refresh" content="0;url={escaped}"></head><body><a href="{escaped}">Open shared session in Warp</a></body></html>"#
        )))
        .unwrap()
}

async fn multi_agent(State(state): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    handle_multi_agent(state, headers, body, false).await
}

async fn passive_suggestions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    handle_multi_agent(state, headers, body, true).await
}

async fn local_command_autocomplete(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let body = match to_bytes(body, MAX_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({ "error": error.to_string() }),
            );
        }
    };
    let request = match serde_json::from_slice::<LocalCommandAutocompleteRequest>(&body) {
        Ok(request) => request,
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({ "error": error.to_string() }),
            );
        }
    };

    state
        .logger
        .log(
            LogLevel::Info,
            "local_command_autocomplete_request",
            json!({
                "prefixChars": request.prefix.chars().count(),
                "historyCount": request.history.len(),
                "fileCandidateCount": request.file_candidates.len(),
                "hasPromptOverride": request.system_prompt_override.is_some(),
            }),
        )
        .await;

    let started = Instant::now();
    match state
        .provider
        .command_autocomplete(
            &state.config,
            &request,
            bearer_token(&headers),
            header_value(&headers, OPENAI_BASE_URL_HEADER),
            header_value(&headers, LOCAL_MODEL_ALIASES_HEADER),
        )
        .await
    {
        Ok(response) => {
            state
                .logger
                .log(
                    LogLevel::Info,
                    "local_command_autocomplete_result",
                    json!({
                        "durationMs": started.elapsed().as_millis(),
                        "source": response.source.as_str(),
                        "parseStatus": response.parse_status.as_str(),
                        "hasCommand": !response.most_likely_action.is_empty(),
                        "commandChars": response.most_likely_action.chars().count(),
                        "rawOutputChars": response.raw_output.chars().count(),
                    }),
                )
                .await;
            json_response(StatusCode::OK, json!(response))
        }
        Err(error) => {
            let status = status_for_local_agent_error(&error);
            state
                .logger
                .log(
                    LogLevel::Warn,
                    "local_command_autocomplete_error",
                    json!({
                        "durationMs": started.elapsed().as_millis(),
                        "status": status.as_u16(),
                        "finishReason": format!("{:?}", error.finish_reason),
                        "model": error.model_name.as_ref(),
                    }),
                )
                .await;
            json_response(
                status,
                json!({ "error": error.message, "model": error.model_name }),
            )
        }
    }
}

async fn graphql(State(state): State<AppState>, uri: Uri, body: Body) -> Response {
    let body = match to_bytes(body, MAX_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({ "error": error.to_string() }),
            );
        }
    };
    let request = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(_)) => serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null),
        Ok(_) => Value::Object(Default::default()),
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({ "error": error.to_string() }),
            );
        }
    };
    let op = uri.query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "op").then_some(value)
        })
    });
    let mut store = state.store.lock().await;
    let result =
        handle_local_graphql_request(request, &mut store, &state.provider, &state.config, op).await;
    let error_messages = graphql_error_messages(&result.payload);
    state
        .logger
        .log(
            if result.status >= 400 || !error_messages.is_empty() {
                LogLevel::Warn
            } else {
                LogLevel::Info
            },
            "graphql_response",
            json!({
                "operationName": result.diagnostics.operation_name,
                "canonicalOperationName": result.diagnostics.canonical_operation_name,
                "statusCode": result.status,
                "errorMessages": error_messages,
            }),
        )
        .await;
    json_response(
        StatusCode::from_u16(result.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        result.payload,
    )
}

async fn handle_multi_agent(
    state: AppState,
    headers: HeaderMap,
    body: Body,
    passive_suggestions: bool,
) -> Response {
    let body = match to_bytes(body, MAX_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({ "error": error.to_string() }),
            );
        }
    };
    let request = match api::Request::decode(body) {
        Ok(request) => request,
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({ "error": error.to_string() }),
            );
        }
    };
    let warp_request = decode_warp_request(request);
    let request_openai_base_url = headers
        .get(OPENAI_BASE_URL_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(non_empty_str)
        .map(str::to_owned);
    let request_local_model_aliases = header_value(&headers, LOCAL_MODEL_ALIASES_HEADER);
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let task_state = state.clone();
    tokio::spawn(async move {
        process_multi_agent(
            task_state,
            tx,
            warp_request,
            request_openai_base_url,
            request_local_model_aliases,
            passive_suggestions,
        )
        .await;
    });

    let stream = stream::unfold(rx, |mut rx| async {
        rx.recv()
            .await
            .map(|chunk| (Ok::<Bytes, Infallible>(Bytes::from(chunk)), rx))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header(header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn process_multi_agent(
    state: AppState,
    tx: mpsc::UnboundedSender<String>,
    warp_request: WarpRequestSummary,
    request_openai_base_url: Option<String>,
    request_local_model_aliases: Option<String>,
    passive_suggestions: bool,
) {
    state
        .logger
        .log(
            LogLevel::Info,
            if passive_suggestions {
                "passive_suggestions_request"
            } else {
                "multi_agent_request"
            },
            json!({
                "conversationId": warp_request.conversation_id,
                "requestId": warp_request.request_id,
                "taskId": warp_request.root_task_id,
                "shouldCreateRootTask": warp_request.should_create_root_task,
                "promptChars": warp_request.prompt.len(),
                "contextChars": warp_request.context_text.as_deref().map(str::len).unwrap_or_default(),
                "contextImageCount": warp_request.context_images.len(),
                "warpModel": warp_request.model,
                "hasRequestApiKey": warp_request.openai_api_key.is_some(),
                "hasOpenAiBaseUrlHeader": request_openai_base_url.is_some(),
                "hasLocalModelAliasesHeader": request_local_model_aliases.is_some(),
            }),
        )
        .await;

    send_event(
        &tx,
        response::stream_init(&warp_request.conversation_id, &warp_request.request_id),
    );

    let mut provider_response_for_usage: Option<ProviderResponse> = None;
    let mut summarized_conversation = false;
    let result = async {
        if passive_suggestions {
            return Ok(());
        }
        if warp_request.should_create_root_task {
            send_event(
                &tx,
                response::create_task(&warp_request.root_task_id, &warp_request.prompt),
            );
        }

        let prepared = prepare_provider_messages(&state, &warp_request).await?;
        let assistant_message_id = uuid::Uuid::new_v4().to_string();
        let streamed_agent_output = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let streamed_for_closure = streamed_agent_output.clone();
        let chunk_tx = tx.clone();
        let provider_response = state
            .provider
            .stream_chat_completion(
                &state.config,
                ChatCompletionParams {
                    messages: if prepared.messages.is_empty() {
                        vec![user_text_message("Continue.")]
                    } else {
                        prepared.messages.clone()
                    },
                    api_key: warp_request.openai_api_key.clone(),
                    base_url: request_openai_base_url,
                    local_model_aliases: request_local_model_aliases,
                    model: warp_request.model.clone(),
                    max_tokens: None,
                    temperature: None,
                    mcp_tools: warp_request.mcp_tools.clone(),
                    enable_tools: !warp_request.is_summarization_request,
                },
                |chunk| {
                    if warp_request.is_summarization_request {
                        return;
                    }
                    let first =
                        !streamed_for_closure.swap(true, std::sync::atomic::Ordering::SeqCst);
                    let event = if first {
                        response::add_agent_output(
                            &assistant_message_id,
                            &warp_request.root_task_id,
                            &warp_request.request_id,
                            &chunk,
                        )
                    } else {
                        response::append_agent_output(
                            &assistant_message_id,
                            &warp_request.root_task_id,
                            &warp_request.request_id,
                            &chunk,
                        )
                    };
                    send_event(&chunk_tx, event);
                },
            )
            .await?;

        if provider_response.content.is_empty() && provider_response.tool_calls.is_empty() {
            let message = if provider_response.suppressed_internal_content {
                "OpenAI-compatible endpoint returned only non-public assistant content."
            } else {
                "OpenAI-compatible endpoint returned no assistant content or tool calls."
            };
            return Err(LocalAgentError::internal(message));
        }

        provider_response_for_usage = Some(provider_response.clone());
        if warp_request.is_summarization_request {
            summarized_conversation = true;
            remember_provider_summarization(
                &state,
                &warp_request.conversation_id,
                &provider_response,
            )
            .await?;
            let messages = state_for_conversation(&state, &warp_request.conversation_id).await?;
            provider_response_for_usage = Some(ProviderResponse {
                context_window_usage: estimate_messages_usage(
                    &messages.messages,
                    provider_response.context_window_tokens,
                ),
                ..provider_response.clone()
            });
        } else {
            remember_provider_response(
                &state,
                &warp_request.conversation_id,
                prepared.pending_messages,
                &provider_response,
            )
            .await?;
        }

        if warp_request.is_summarization_request {
            send_event(
                &tx,
                response::add_conversation_summary(
                    &assistant_message_id,
                    &warp_request.root_task_id,
                    &warp_request.request_id,
                    &provider_response.content,
                    response::summary_token_count(&provider_response.content),
                ),
            );
        } else if !provider_response.content.is_empty()
            && !streamed_agent_output.load(std::sync::atomic::Ordering::SeqCst)
        {
            send_event(
                &tx,
                response::add_agent_output(
                    &assistant_message_id,
                    &warp_request.root_task_id,
                    &warp_request.request_id,
                    &provider_response.content,
                ),
            );
        }

        for tool_call in &provider_response.tool_calls {
            let parsed = response::parse_tool_call(tool_call, &warp_request.mcp_tools)
                .map_err(|error| LocalAgentError::internal(error.to_string()))?
                .ok_or_else(|| {
                    LocalAgentError::internal(format!(
                        "Unsupported provider tool call: {}",
                        tool_call.name
                    ))
                })?;
            send_event(
                &tx,
                response::add_tool_call(
                    &uuid::Uuid::new_v4().to_string(),
                    &warp_request.root_task_id,
                    &warp_request.request_id,
                    parsed,
                ),
            );
        }

        Ok(())
    }
    .await;

    match result {
        Ok(()) => send_event(
            &tx,
            response::stream_finished_done(
                provider_response_for_usage.and_then(|response| response.context_window_usage),
                summarized_conversation,
            ),
        ),
        Err(error) => {
            state
                .logger
                .log(
                    LogLevel::Error,
                    "multi_agent_error",
                    json!({
                        "requestId": warp_request.request_id,
                        "message": error.message,
                    }),
                )
                .await;
            send_event(&tx, stream_finished_for_error(&error));
        }
    }
}

struct PreparedProviderMessages {
    messages: Vec<ProviderChatMessage>,
    pending_messages: Vec<ProviderChatMessage>,
}

async fn prepare_provider_messages(
    state: &AppState,
    warp_request: &WarpRequestSummary,
) -> Result<PreparedProviderMessages, LocalAgentError> {
    let conversation = state_for_conversation(state, &warp_request.conversation_id).await?;
    let pending_messages = pending_provider_messages(warp_request);
    Ok(PreparedProviderMessages {
        messages: conversation
            .messages
            .into_iter()
            .chain(pending_messages.clone())
            .collect(),
        pending_messages,
    })
}

fn pending_provider_messages(warp_request: &WarpRequestSummary) -> Vec<ProviderChatMessage> {
    if warp_request.is_summarization_request {
        return vec![user_text_message(
            format_summarization_request_for_provider(warp_request),
        )];
    }
    if !warp_request.tool_results.is_empty() {
        return warp_request
            .tool_results
            .iter()
            .map(|result| {
                tool_result_message(
                    tool_call_id(result).to_owned(),
                    format_tool_result_for_provider(result),
                )
            })
            .collect();
    }
    if !warp_request.prompt.is_empty()
        || warp_request.context_text.is_some()
        || !warp_request.context_images.is_empty()
    {
        return vec![user_text_message(content_with_images(
            format_user_message_for_provider(warp_request),
            &warp_request.context_images,
        ))];
    }
    Vec::new()
}

fn format_user_message_for_provider(warp_request: &WarpRequestSummary) -> String {
    let prompt = if warp_request.prompt.is_empty() {
        "Please use the attached context."
    } else {
        &warp_request.prompt
    };
    match warp_request.context_text.as_deref() {
        Some(context) => format!("Attached context:\n{context}\n\nUser request:\n{prompt}"),
        None => prompt.to_owned(),
    }
}

fn format_summarization_request_for_provider(warp_request: &WarpRequestSummary) -> String {
    [
        Some("Summarize the conversation so far into a compact handoff for continuing the same task.".to_owned()),
        Some("Preserve current goals, decisions, constraints, important file paths, commands, errors, and outstanding next steps.".to_owned()),
        Some("Omit repetitive transcript detail and keep the summary dense.".to_owned()),
        warp_request
            .summarization_prompt
            .as_ref()
            .map(|prompt| format!("Additional user instruction:\n{prompt}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n\n")
}

fn format_compacted_conversation_summary(summary: &str) -> String {
    format!(
        "The conversation before this point was compacted.\nSummary:\n{}",
        summary.trim()
    )
}

async fn state_for_conversation(
    state: &AppState,
    conversation_id: &str,
) -> Result<ConversationState, LocalAgentError> {
    if let Some(existing) = state
        .conversations
        .lock()
        .await
        .get(conversation_id)
        .cloned()
    {
        return Ok(existing);
    }
    let persisted = state
        .store
        .lock()
        .await
        .get_ai_conversation(conversation_id)
        .map_err(|error| LocalAgentError::internal(error.to_string()))?;
    let loaded = persisted
        .map(|record| ConversationState {
            messages: messages_from_stored_conversation(
                &record.messages,
                state.config.local_max_history_messages,
            ),
        })
        .unwrap_or_default();
    state
        .conversations
        .lock()
        .await
        .insert(conversation_id.to_owned(), loaded.clone());
    Ok(loaded)
}

async fn remember_provider_response(
    state: &AppState,
    conversation_id: &str,
    pending_messages: Vec<ProviderChatMessage>,
    provider_response: &ProviderResponse,
) -> Result<(), LocalAgentError> {
    let messages = {
        let mut conversations = state.conversations.lock().await;
        let conversation = conversations.entry(conversation_id.to_owned()).or_default();
        conversation.messages.extend(pending_messages);
        conversation.messages.push(assistant_message(
            provider_response.content.clone(),
            provider_response.tool_calls.clone(),
        ));
        trim_conversation_state(conversation, state.config.local_max_history_messages);
        conversation.messages.clone()
    };
    persist_conversation_state(state, conversation_id, &messages).await
}

async fn remember_provider_summarization(
    state: &AppState,
    conversation_id: &str,
    provider_response: &ProviderResponse,
) -> Result<(), LocalAgentError> {
    let messages = vec![system_message(format_compacted_conversation_summary(
        &provider_response.content,
    ))];
    state.conversations.lock().await.insert(
        conversation_id.to_owned(),
        ConversationState {
            messages: messages.clone(),
        },
    );
    persist_conversation_state(state, conversation_id, &messages).await
}

async fn persist_conversation_state(
    state: &AppState,
    conversation_id: &str,
    messages: &[ProviderChatMessage],
) -> Result<(), LocalAgentError> {
    let messages = provider_messages_to_json(messages);
    state
        .store
        .lock()
        .await
        .upsert_ai_conversation(conversation_id, &messages)
        .map_err(|error| LocalAgentError::internal(error.to_string()))?;
    Ok(())
}

fn trim_conversation_state(state: &mut ConversationState, max_messages: usize) {
    if state.messages.len() > max_messages {
        state.messages.drain(0..state.messages.len() - max_messages);
    }
}

async fn load_conversation_state(
    store: &mut IntegrationStore,
    max_messages: usize,
    logger: &Logger,
) -> HashMap<String, ConversationState> {
    match store.list_ai_conversations() {
        Ok(conversations) => {
            let loaded = conversations
                .into_iter()
                .map(|conversation| {
                    (
                        conversation.conversation_id,
                        ConversationState {
                            messages: messages_from_stored_conversation(
                                &conversation.messages,
                                max_messages,
                            ),
                        },
                    )
                })
                .collect::<HashMap<_, _>>();
            logger
                .log(
                    LogLevel::Info,
                    "conversation_state_loaded",
                    json!({ "conversationCount": loaded.len() }),
                )
                .await;
            loaded
        }
        Err(error) => {
            logger
                .log(
                    LogLevel::Warn,
                    "conversation_state_load_failed",
                    json!({ "message": error.to_string() }),
                )
                .await;
            HashMap::new()
        }
    }
}

fn stream_finished_for_error(error: &LocalAgentError) -> api::ResponseEvent {
    match error.finish_reason {
        FinishReason::InvalidApiKey => {
            response::stream_finished_invalid_api_key(error.model_name.as_deref())
        }
        FinishReason::LlmUnavailable => response::stream_finished_llm_unavailable(),
        FinishReason::ContextWindowExceeded => response::stream_finished_context_window_exceeded(),
        FinishReason::QuotaLimit => response::stream_finished_quota_limit(),
        FinishReason::InternalError => response::stream_finished_internal_error(&error.message),
    }
}

fn send_event(tx: &mpsc::UnboundedSender<String>, event: api::ResponseEvent) {
    let _ = tx.send(response::format_sse_data_event(&event));
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(non_empty_str)
        .map(str::to_owned)
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(non_empty_str)
        .map(str::to_owned)
}

fn status_for_local_agent_error(error: &LocalAgentError) -> StatusCode {
    match error.finish_reason {
        FinishReason::InvalidApiKey => StatusCode::UNAUTHORIZED,
        FinishReason::QuotaLimit => StatusCode::TOO_MANY_REQUESTS,
        FinishReason::ContextWindowExceeded => StatusCode::PAYLOAD_TOO_LARGE,
        FinishReason::LlmUnavailable | FinishReason::InternalError => StatusCode::BAD_GATEWAY,
    }
}

fn json_response(status: StatusCode, payload: Value) -> Response {
    (status, axum::Json(payload)).into_response()
}

fn tool_call_id(result: &WarpToolResult) -> &str {
    match result {
        WarpToolResult::ReadFiles { tool_call_id, .. }
        | WarpToolResult::RunShellCommand { tool_call_id, .. }
        | WarpToolResult::Grep { tool_call_id, .. }
        | WarpToolResult::FileGlob { tool_call_id, .. }
        | WarpToolResult::ApplyFileDiffs { tool_call_id, .. }
        | WarpToolResult::SuggestPlan { tool_call_id, .. }
        | WarpToolResult::Generic { tool_call_id, .. } => tool_call_id,
    }
}

fn format_tool_result_for_provider(result: &WarpToolResult) -> String {
    if let Some(error) = tool_error(result) {
        return format!("Error: {error}");
    }
    match result {
        WarpToolResult::ReadFiles { files, .. } => files
            .iter()
            .map(|file| format!("File: {}\n{}", file.file_path, file.content))
            .collect::<Vec<_>>()
            .join("\n\n"),
        WarpToolResult::RunShellCommand {
            command,
            output,
            exit_code,
            ..
        } => format!(
            "Command: {}\nExit code: {}\nOutput:\n{}",
            command.as_deref().unwrap_or(""),
            exit_code.map(|code| code.to_string()).unwrap_or_default(),
            output.as_deref().unwrap_or("")
        ),
        WarpToolResult::Grep { matched_files, .. } => matched_files
            .iter()
            .map(|file| {
                if file.line_numbers.is_empty() {
                    file.file_path.clone()
                } else {
                    format!(
                        "{} lines {}",
                        file.file_path,
                        file.line_numbers
                            .iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        WarpToolResult::FileGlob {
            matched_files,
            warnings,
            ..
        } => format!(
            "{}{}",
            matched_files.join("\n"),
            warnings
                .as_ref()
                .map(|warnings| format!("\nWarnings:\n{warnings}"))
                .unwrap_or_default()
        ),
        WarpToolResult::ApplyFileDiffs {
            updated_files,
            deleted_files,
            ..
        } => format!(
            "Updated files:\n{}\nDeleted files:\n{}",
            updated_files.join("\n"),
            deleted_files.join("\n")
        ),
        WarpToolResult::SuggestPlan {
            status, plan_text, ..
        } => format!(
            "Status: {}{}",
            match status {
                SuggestPlanStatus::Accepted => "accepted",
                SuggestPlanStatus::Edited => "edited",
            },
            plan_text
                .as_ref()
                .map(|plan| format!("\nPlan:\n{plan}"))
                .unwrap_or_default()
        ),
        WarpToolResult::Generic { name, content, .. } => format!("{name} result:\n{content}"),
    }
}

fn tool_error(result: &WarpToolResult) -> Option<&str> {
    match result {
        WarpToolResult::ReadFiles { error, .. }
        | WarpToolResult::RunShellCommand { error, .. }
        | WarpToolResult::Grep { error, .. }
        | WarpToolResult::FileGlob { error, .. }
        | WarpToolResult::ApplyFileDiffs { error, .. }
        | WarpToolResult::Generic { error, .. } => {
            error.as_deref().filter(|error| !error.is_empty())
        }
        WarpToolResult::SuggestPlan { .. } => None,
    }
}

fn estimate_messages_usage(
    messages: &[ProviderChatMessage],
    context_window_tokens: Option<u32>,
) -> Option<f32> {
    let context_window_tokens = context_window_tokens?;
    let chars: usize = serde_json::to_string(messages).unwrap_or_default().len();
    Some(((chars.div_ceil(4) as f32) / context_window_tokens as f32).min(1.0))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
