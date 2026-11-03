//! The streaming turn loop: one model call per [`run_turn`], dispatched on the
//! endpoint's wire schema and mapped onto neutral [`AgentEvent`]s.
//! Continuation after tool execution is the host's job — it calls `run_turn`
//! again with the tool results appended to the transcript, mirroring how the
//! Warp server loop is driven.

use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use reqwest_eventsource::{Event, EventSource};

use crate::{
    AgentEvent, ChatMessage, EndpointSchema, LocalAgentError, LocalEndpointConfig, ToolDefinition,
    openai, responses,
};

/// How long the provider may go without sending an event before the stream
/// counts as stalled.
///
/// A reasoning model can pause for a while, so the deadline is generous; it
/// exists so a provider that stops answering leaves the turn with a
/// retryable error instead of pending until the user cancels it.
pub(crate) const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// What the event stream produced next.
pub(crate) enum NextEvent {
    /// An event, or the failure the stream reported.
    Event(Result<Event, reqwest_eventsource::Error>),
    /// The stream ended.
    Ended,
    /// The provider sent nothing for [`SSE_IDLE_TIMEOUT`].
    Idle,
}

/// Reads the next event of the stream.
///
/// The wait is bounded by [`SSE_IDLE_TIMEOUT`], which is per event and not a
/// deadline on the whole response, so a long reasoning pause is fine but a
/// silent provider is not.
pub(crate) async fn next_event(source: &mut EventSource) -> NextEvent {
    let next = Box::pin(source.next());
    let idle = Box::pin(async_io::Timer::after(SSE_IDLE_TIMEOUT));
    match futures::future::select(next, idle).await {
        futures::future::Either::Left((event, _idle)) => match event {
            Some(event) => NextEvent::Event(event),
            None => NextEvent::Ended,
        },
        futures::future::Either::Right((_idle, _next)) => NextEvent::Idle,
    }
}

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
/// redirect rules, network policy) belongs in this one builder. It follows the
/// network policy: redirects are re-checked against it, and in local-only mode
/// system proxies are not used. A redirect carries the request body (the
/// transcript), so an endpoint must not be able to bounce it to a host the
/// policy would have refused. Building can fail, and a plain client in its place
/// would drop that protection, so the failure is reported instead.
fn client() -> Result<&'static reqwest::Client, LocalAgentError> {
    // Reuse a single `reqwest::Client` across turns so TCP and TLS
    // sessions are pooled across requests.
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = network_policy::apply_to_client_builder(reqwest::Client::builder())
        .build()
        .map_err(|err| LocalAgentError::Transport(err.without_url().to_string()))?;
    Ok(CLIENT.get_or_init(|| client))
}

/// Opens the event stream for one JSON POST to `url`. Every wire schema's
/// request goes through here, so the schema supplies only the URL, the body,
/// and any extra headers.
///
/// The network policy is checked here, before anything is built, so no wire
/// schema can reach an endpoint the policy refuses. The denial is reported
/// without the URL, which may carry credentials.
/// Retrying is left to the caller, so the SSE layer never reconnects itself.
pub(crate) fn post_event_source(
    url: &str,
    body: &impl serde::Serialize,
    api_key: &str,
    extra_headers: &[(&str, &str)],
) -> Result<EventSource, LocalAgentError> {
    network_policy::check_url_str(url, "local agent request")
        .map_err(|denied| crate::redact_denial(&denied))?;
    let mut builder = client()?.post(url).json(body);
    if !api_key.trim().is_empty() {
        builder = builder.bearer_auth(api_key.trim());
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    let mut source = EventSource::new(buildable(builder)?)
        .map_err(|err| LocalAgentError::Transport(err.to_string()))?;
    source.set_retry_policy(Box::new(openai::NeverRetry));
    Ok(source)
}

/// Surfaces a misconfigured request (a URL without a scheme, a key that is not
/// a valid header value) as the permanent error it is. A builder holding such
/// an error cannot be cloned, which `EventSource::new` would report as an
/// opaque, seemingly transient transport failure.
///
/// The text deliberately omits the request URL (it may carry credentials in
/// its query) and everything about the key.
fn buildable(builder: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder, LocalAgentError> {
    let (client, request) = builder.build_split();
    let request = request.map_err(|err| {
        LocalAgentError::Parse(format!(
            "the request could not be built; check the base URL and API key: {}",
            error_chain(&err.without_url())
        ))
    })?;
    Ok(reqwest::RequestBuilder::from_parts(client, request))
}

/// An error with its causes appended (`reqwest`'s own text is just "builder
/// error"; the useful part is the source).
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
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
        loop {
            match next_event(&mut source).await {
                NextEvent::Event(Ok(Event::Open)) => {}
                NextEvent::Event(Ok(Event::Message(message))) => {
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
                NextEvent::Event(Err(reqwest_eventsource::Error::StreamEnded)) => break,
                NextEvent::Event(Err(err)) => {
                    Err(openai::map_event_source_error(err, &config).await)?;
                }
                NextEvent::Ended => break,
                NextEvent::Idle => {
                    Err(LocalAgentError::Transport(format!(
                        "the provider sent no event for {:?} before the response completed",
                        SSE_IDLE_TIMEOUT
                    )))?;
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
