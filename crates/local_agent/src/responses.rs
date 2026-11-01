//! The Responses API turn loop: one streamed `POST /responses` per
//! [`run_turn`], mapped onto neutral [`AgentEvent`]s. Like the Chat
//! Completions engine, it performs a single model call; the host executes the
//! emitted tool calls and calls `run_turn` again with the results appended.
//!
//! The request is stateless (`store: false`): the full transcript is replayed
//! each turn, with the reasoning items the provider returned (see
//! [`AgentEvent::ReplayItem`]) passed back verbatim.
//!
//! A turn that fails before it has yielded any event is retried a bounded
//! number of times on transient errors. Every phase, including the back-off
//! sleep, lives inside the returned stream, so dropping it cancels the
//! request.

mod model;
mod request;
mod retry;
mod stream;
mod tools;

use futures::StreamExt;
use futures::stream::BoxStream;
use reqwest_eventsource::Event;

use crate::{
    AgentEvent, ChatMessage, LocalAgentError, LocalEndpointConfig, ToolDefinition, engine, openai,
};
use request::ResponsesRequest;
use retry::RetryPolicy;

pub(crate) fn run_turn(
    transcript: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    config: LocalEndpointConfig,
) -> BoxStream<'static, Result<AgentEvent, LocalAgentError>> {
    run_turn_with_policy(transcript, tools, config, RetryPolicy::default())
}

fn run_turn_with_policy(
    transcript: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    config: LocalEndpointConfig,
    policy: RetryPolicy,
) -> BoxStream<'static, Result<AgentEvent, LocalAgentError>> {
    Box::pin(async_stream::try_stream! {
        let request = request::build_request(&transcript, &tools, &config);
        let mut attempt = 1;
        let mut yielded_any = false;
        'attempts: loop {
            let mut events = attempt_events(&request, &config);
            while let Some(item) = events.next().await {
                match item {
                    Ok(event) => {
                        yielded_any = true;
                        yield event;
                    }
                    Err(err) if !yielded_any && policy.should_retry(attempt, &err) => {
                        // Release the failed attempt's connection before waiting.
                        drop(events);
                        let delay = policy.backoff(attempt);
                        log::warn!(
                            "Responses request failed on attempt {attempt}/{}, retrying in {delay:?}: {err}",
                            policy.max_attempts
                        );
                        async_io::Timer::after(delay).await;
                        attempt += 1;
                        continue 'attempts;
                    }
                    Err(err) => {
                        Err(err)?;
                    }
                }
            }
            return;
        }
    })
}

/// One attempt: opens the event stream and yields events up to and including
/// [`AgentEvent::Done`]. Dropping the stream drops the `EventSource`, which
/// aborts the connection.
fn attempt_events(
    request: &ResponsesRequest,
    config: &LocalEndpointConfig,
) -> BoxStream<'static, Result<AgentEvent, LocalAgentError>> {
    // Retrying is `run_turn_with_policy`'s job, where it can be bounded and
    // only happens before anything was yielded.
    let session_header = request
        .session_id()
        .map(|session_id| (request::SESSION_ID_HEADER, session_id));
    let source = engine::post_event_source(
        &request::endpoint_url(&config.base_url),
        request,
        &config.api_key,
        session_header.as_slice(),
    );
    let config = config.clone();
    Box::pin(async_stream::try_stream! {
        let mut source = source?;

        let mut response = stream::ResponseStream::default();
        while let Some(event) = source.next().await {
            match event {
                Ok(Event::Open) => {}
                Ok(Event::Message(message)) => {
                    let step = response.ingest(&message.event, &message.data, &config)?;
                    for event in step.events {
                        yield event;
                    }
                    if step.finished {
                        source.close();
                        yield AgentEvent::Done;
                        return;
                    }
                }
                Err(reqwest_eventsource::Error::StreamEnded) => break,
                Err(err) => {
                    Err(openai::map_event_source_error(err, &config).await)?;
                }
            }
        }
        Err(LocalAgentError::Transport(
            "stream ended before the response completed".to_string(),
        ))?;
    })
}

#[cfg(test)]
#[path = "responses_tests.rs"]
mod tests;
