//! OpenAI Responses API adapter shared by API keys and ChatGPT plan usage.
//!
//! Wire semantics are based on the public Responses documentation and OpenAI
//! Codex (Apache-2.0), revision 970111f6cb62cc02fe36a82ea7d71b37a2889c97.
//! Peri owns transport, retry, cancellation and tool execution.

mod replay;
mod request;
mod request_content;
mod response;
mod stream;
mod stream_item;

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    runtime::stream::runtime_http_sse_stream,
    transport::{HttpRequest, HttpTransport, ReqwestTransport},
    ModelCapabilities, ModelError, ModelRequest, ModelResult, ModelRuntimeConfig, ModelStream,
    PreparedModelCall, PreparedModelRequest, ProtocolErrorKind, RequestAuth, RequestCredential,
};

use request::BuiltResponsesRequest;

pub(super) const PROVIDER_NAME: &str = "openai_responses";
pub(super) const TOOL_NAMESPACE: &str = "peri";

/// Request capabilities differ even when both profiles use `/v1/responses`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResponsesProfile {
    #[default]
    Platform,
    #[serde(rename = "chatgpt_plan")]
    ChatGptPlan,
}

/// Explicit Responses configuration. Credential acquisition belongs to the host.
pub struct ResponsesConfig {
    endpoint: Url,
    bearer: String,
    auth: Option<Arc<dyn RequestAuth>>,
    model: String,
    profile: ResponsesProfile,
    credential_ref: String,
    reasoning_effort: Option<String>,
    max_tokens: Option<u32>,
    context_window: Option<u32>,
    runtime: ModelRuntimeConfig,
}

impl ResponsesConfig {
    pub fn new(endpoint: Url, bearer: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint,
            bearer: bearer.into(),
            auth: None,
            model: model.into(),
            profile: ResponsesProfile::Platform,
            // Hosts should supply a durable account/grant identity. Random is
            // a safe fallback: it can only reduce replay reuse across restarts.
            credential_ref: format!("ephemeral:{:032x}", rand::random::<u128>()),
            reasoning_effort: None,
            max_tokens: None,
            context_window: None,
            runtime: ModelRuntimeConfig::default(),
        }
    }

    pub fn with_profile(mut self, profile: ResponsesProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Stable account/grant identity, never the access or refresh token itself.
    pub fn with_credential_ref(mut self, credential_ref: impl Into<String>) -> Self {
        if self.auth.is_none() {
            self.credential_ref = credential_ref.into();
        }
        self
    }

    pub fn with_auth(mut self, auth: Arc<dyn RequestAuth>) -> Self {
        self.credential_ref = auth.credential_ref().to_owned();
        self.auth = Some(auth);
        self.bearer.clear();
        self
    }

    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_context_window(mut self, context_window: Option<u32>) -> Self {
        self.context_window = context_window;
        self
    }

    pub fn with_runtime(mut self, runtime: ModelRuntimeConfig) -> Self {
        self.runtime = runtime;
        self
    }
}

impl fmt::Debug for ResponsesConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponsesConfig")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("profile", &self.profile)
            .field("credential_ref", &self.credential_ref)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("max_tokens", &self.max_tokens)
            .field("context_window", &self.context_window)
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

pub struct ResponsesModel {
    config: ResponsesConfig,
    transport: Arc<dyn HttpTransport>,
    client: reqwest::Client,
}

impl ResponsesModel {
    pub fn new(config: ResponsesConfig) -> Self {
        // In particular, never forward a ChatGPT grant to a redirect target.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Responses HTTP client construction");
        Self {
            config,
            transport: Arc::new(ReqwestTransport::new(client.clone())),
            client,
        }
    }

    #[cfg(test)]
    fn with_transport(config: ResponsesConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            config,
            transport,
            client: reqwest::Client::new(),
        }
    }

    fn native_request(
        client: &reqwest::Client,
        bearer: &str,
        built: &BuiltResponsesRequest,
    ) -> ModelResult<HttpRequest> {
        let request = client
            .post(built.endpoint.clone())
            .bearer_auth(bearer)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&built.body)
            .build()
            .map_err(|error| {
                ModelError::protocol(ProtocolErrorKind::Provider).with_error(&error)
            })?;
        Ok(HttpRequest::new(request))
    }
}

#[async_trait]
impl crate::Model for ResponsesModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_reasoning: true,
            supports_vision: true,
            supports_streaming: true,
        }
    }

    fn output_token_limit(&self) -> Option<u32> {
        match self.config.profile {
            ResponsesProfile::Platform => self.config.max_tokens,
            ResponsesProfile::ChatGptPlan => None,
        }
    }

    fn context_window(&self) -> Option<u32> {
        self.config.context_window
    }

    fn prepare_request(&self, request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        request::build_request(&self.config, request)?.observe(&self.config.runtime)
    }

    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<PreparedModelCall> {
        let built = Arc::new(request::build_request(&self.config, &request)?);
        let checkpoint = serde_json::json!({
            "provider": PROVIDER_NAME,
            "endpoint": built.endpoint.as_str(),
            "model": self.config.model,
            "credentialRef": self.config.credential_ref,
            "body": built.body,
        });
        let runtime = self.config.runtime.clone();
        let client = self.client.clone();
        let bearer = self.config.bearer.clone();
        let transport = Arc::clone(&self.transport);
        let decoders = stream::decoders(built.binding.clone(), built.tool_names.clone());
        if let Some(auth) = self.config.auth.clone() {
            let endpoint_auth = Arc::clone(&auth);
            let request_factory = Arc::new(move |credential: &RequestCredential| {
                endpoint_auth.validate_endpoint(&built.endpoint)?;
                Self::native_request(&client, credential.bearer(), &built)
            });
            return Ok(PreparedModelCall::new(checkpoint, move |cancellation| {
                Ok(
                    crate::runtime::stream::runtime_authenticated_http_sse_stream(
                        &runtime,
                        cancellation,
                        transport,
                        request_factory,
                        Arc::<str>::from(PROVIDER_NAME),
                        decoders,
                        auth,
                    ),
                )
            }));
        }
        let request_factory = Arc::new(move || Self::native_request(&client, &bearer, &built));
        Ok(PreparedModelCall::new(checkpoint, move |cancellation| {
            Ok(runtime_http_sse_stream(
                &runtime,
                cancellation,
                transport,
                request_factory,
                Arc::<str>::from(PROVIDER_NAME),
                decoders,
            ))
        }))
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        if cancellation.is_cancelled() {
            return Err(ModelError::cancelled());
        }
        self.prepare_stream(request)?.start(cancellation)
    }
}

#[cfg(test)]
#[path = "request_test.rs"]
mod request_test;

#[cfg(test)]
#[path = "stream_test.rs"]
mod stream_test;

#[cfg(test)]
#[path = "stream_incomplete_test.rs"]
mod stream_incomplete_test;
