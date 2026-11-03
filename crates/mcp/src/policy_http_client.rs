//! A Streamable HTTP client that checks the network policy on every request.
//!
//! The policy is checked once when an MCP server connects, and its redirects are re-checked by the
//! reqwest client. Neither covers a policy that changes while the connection lives: a host removed
//! from the allow-list in Settings would keep receiving requests from an open transport. This
//! wrapper checks the current policy before each request instead, as the legacy SSE client does.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use network_policy::ServicesMode;
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::streamable_http_client::{
    SseError, StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::Sse;

const REQUEST_PURPOSE: &str = "MCP request";

/// Reported when a request is refused. Names the rule, never the URL, which can carry credentials.
const REQUEST_DENIED_MESSAGE: &str = "the network policy no longer allows this MCP server: local-only services mode only allows loopback addresses and the private hosts listed in Settings";

/// Wraps `C` so that every request first passes the current network policy.
#[derive(Clone)]
pub(crate) struct PolicyCheckedClient<C>(pub(crate) C);

fn check_request<E>(mode: ServicesMode, uri: &str) -> Result<(), StreamableHttpError<E>>
where
    E: std::error::Error + Send + Sync + 'static,
{
    network_policy::check_url_str_for_mode(mode, uri, REQUEST_PURPOSE).map_err(|_| {
        StreamableHttpError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            REQUEST_DENIED_MESSAGE,
        ))
    })
}

fn check_current_policy<E>(uri: &str) -> Result<(), StreamableHttpError<E>>
where
    E: std::error::Error + Send + Sync + 'static,
{
    check_request(network_policy::services_mode(), uri)
}

// `Sync` is needed because the trait's futures must be `Send`, and they borrow `&self`.
impl<C: StreamableHttpClient + Sync> StreamableHttpClient for PolicyCheckedClient<C> {
    type Error = C::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        check_current_policy(&uri)?;
        self.0
            .post_message(uri, message, session_id, auth_header, custom_headers)
            .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        check_current_policy(&uri)?;
        self.0
            .post_message_with_max_sse_event_size(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        check_current_policy(&uri)?;
        self.0
            .delete_session(uri, session_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        check_current_policy(&uri)?;
        self.0
            .get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        check_current_policy(&uri)?;
        self.0
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .await
    }
}

#[cfg(test)]
#[path = "policy_http_client_tests.rs"]
mod tests;
