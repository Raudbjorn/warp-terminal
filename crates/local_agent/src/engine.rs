//! The streaming turn loop: one Chat Completions call per [`run_turn`],
//! mapped onto neutral [`AgentEvent`]s. Continuation after tool execution is
//! the host's job — it calls `run_turn` again with the tool results appended
//! to the transcript, mirroring how the Warp server loop is driven.

use futures::StreamExt;
use futures::stream::BoxStream;
use reqwest_eventsource::{Event, EventSource};

use crate::{
    AgentEvent, ChatMessage, LocalAgentError, LocalEndpointConfig, ToolDefinition, openai,
};

/// A POST to `url` on the shared HTTP client, authenticated with the
/// endpoint's API key when one is configured.
pub(crate) fn authorized_post(url: &str, config: &LocalEndpointConfig) -> reqwest::RequestBuilder {
    // Reuse a single `reqwest::Client` across turns so TCP and TLS
    // sessions are pooled across requests.
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = CLIENT.get_or_init(reqwest::Client::new);
    let builder = client.post(url);
    if config.api_key.trim().is_empty() {
        builder
    } else {
        builder.bearer_auth(config.api_key.trim())
    }
}

pub(crate) fn run_turn(
    transcript: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    config: LocalEndpointConfig,
) -> BoxStream<'static, Result<AgentEvent, LocalAgentError>> {
    Box::pin(async_stream::try_stream! {
        let request = openai::build_request(&transcript, &tools, &config);
        let builder = authorized_post(&openai::endpoint_url(&config.base_url), &config)
            .json(&request);
        let mut source = EventSource::new(builder)
            .map_err(|err| LocalAgentError::Transport(err.to_string()))?;
        source.set_retry_policy(Box::new(openai::NeverRetry));

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
