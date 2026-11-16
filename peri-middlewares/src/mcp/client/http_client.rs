//! MCP streamable HTTP client dispatch.
//!
//! Plain and OAuth-authorized transports differ only in how the request is
//! authorized. Erasing that difference before the transport worker keeps a
//! single monomorphized HTTP stack while preserving the real `AuthClient`
//! challenge/refresh behavior.

use std::collections::HashMap;
use std::sync::Arc;

use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::auth::AuthClient;
use rmcp::transport::common::client_side_sse::BoxedSseResponse;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};

/// HTTP backend used by every remote MCP transport in this process.
#[derive(Clone)]
pub(crate) enum PeriHttpClient {
    Plain(reqwest::Client),
    Authed(AuthClient<reqwest::Client>),
}

pub(crate) type PeriHttpTransport = rmcp::transport::StreamableHttpClientTransport<PeriHttpClient>;

impl StreamableHttpClient for PeriHttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        match self {
            Self::Plain(client) => {
                client
                    .post_message(uri, message, session_id, auth_header, custom_headers)
                    .await
            }
            Self::Authed(client) => {
                client
                    .post_message(uri, message, session_id, auth_header, custom_headers)
                    .await
            }
        }
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
        match self {
            Self::Plain(client) => {
                client
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
            Self::Authed(client) => {
                client
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
        }
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxedSseResponse, StreamableHttpError<Self::Error>> {
        match self {
            Self::Plain(client) => {
                client
                    .get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
                    .await
            }
            Self::Authed(client) => {
                client
                    .get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
                    .await
            }
        }
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxedSseResponse, StreamableHttpError<Self::Error>> {
        match self {
            Self::Plain(client) => {
                client
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
            Self::Authed(client) => {
                client
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
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        match self {
            Self::Plain(client) => {
                client
                    .delete_session(uri, session_id, auth_header, custom_headers)
                    .await
            }
            Self::Authed(client) => {
                client
                    .delete_session(uri, session_id, auth_header, custom_headers)
                    .await
            }
        }
    }
}

#[cfg(test)]
#[path = "http_client_test.rs"]
mod tests;
