//! The streaming turn loop: one model call per [`run_turn`], dispatched on the
//! endpoint's wire schema and mapped onto neutral [`AgentEvent`]s.
//! Continuation after tool execution is the host's job — it calls `run_turn`
//! again with the tool results appended to the transcript, mirroring how the
//! Warp server loop is driven.

use futures::StreamExt;
use futures::stream::BoxStream;
use reqwest_eventsource::{Event, EventSource};

use crate::{
    AgentEvent, ChatMessage, EndpointSchema, LocalAgentError, LocalEndpointConfig, ToolDefinition,
    openai, responses,
};

pub(crate) fn run_turn(
    transcript: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    config: LocalEndpointConfig,
) -> BoxStream<'static, Result<AgentEvent, LocalAgentError>> {
    match config.schema {
        EndpointSchema::ChatCompletions => run_chat_turn(transcript, tools, config),
        EndpointSchema::Responses => responses::run_turn(transcript, tools, config),
    }
}

/// The process-wide HTTP client, shared by every wire schema.
///
/// Policy seam: anything that must hold for all outbound requests (proxy and
/// redirect rules, network policy) belongs in this one builder.
fn client() -> &'static reqwest::Client {
    // Reuse a single `reqwest::Client` across turns so TCP and TLS
    // sessions are pooled across requests.
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Opens the event stream for one JSON POST to `url`. Every wire schema's
/// request goes through here, so the schema supplies only the URL, the body,
/// and any extra headers.
///
/// Policy seam: a per-request URL check belongs at the top of this function.
/// Retrying is left to the caller, so the SSE layer never reconnects itself.
pub(crate) fn post_event_source(
    url: &str,
    body: &impl serde::Serialize,
    api_key: &str,
    extra_headers: &[(&str, &str)],
) -> Result<EventSource, LocalAgentError> {
    let mut builder = client().post(url).json(body);
    if !api_key.trim().is_empty() {
        builder = builder.bearer_auth(api_key.trim());
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    let mut source =
        EventSource::new(builder).map_err(|err| LocalAgentError::Transport(err.to_string()))?;
    source.set_retry_policy(Box::new(openai::NeverRetry));
    Ok(source)
}

fn run_chat_turn(
    transcript: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    config: LocalEndpointConfig,
) -> BoxStream<'static, Result<AgentEvent, LocalAgentError>> {
    Box::pin(async_stream::try_stream! {
        let request = openai::build_request(&transcript, &tools, &config);
        let mut source = post_event_source(
            &openai::endpoint_url(&config.base_url),
            &request,
            &config.api_key,
            &[],
        )?;

        let mut accumulator = openai::StreamAccumulator::default();
        // Dropping this stream drops `source`, aborting the connection — that
        // is the cancellation path.
        while let Some(event) = source.next().await {
            match event {
                Ok(Event::Open) => {}
                Ok(Event::Message(message)) => {
                    if message.data.trim() == "[DONE]" {
                        source.close();
                        for event in accumulator.flush_tool_calls()? {
                            yield event;
                        }
                        yield AgentEvent::Done;
                        return;
                    }
                    for event in accumulator.ingest(openai::parse_chunk(&message.data)?)? {
                        yield event;
                    }
                }
                // `StreamEnded` is how the EventSource reports EOF. Some
                // servers close without sending `[DONE]`; treat that as a
                // normal end if the model already reported a finish_reason.
                Err(reqwest_eventsource::Error::StreamEnded) => break,
                Err(err) => {
                    Err(openai::map_event_source_error(err, &config).await)?;
                }
            }
        }

        if accumulator.finished() {
            for event in accumulator.flush_tool_calls()? {
                yield event;
            }
            yield AgentEvent::Done;
        } else {
            Err(LocalAgentError::Transport(
                "stream ended before the model finished responding".to_string(),
            ))?;
        }
    })
}
