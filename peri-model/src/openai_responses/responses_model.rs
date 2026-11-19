use super::async_trait;
use super::fmt;
use super::request;
use super::stream;
use super::Arc;
use super::BuiltResponsesRequest;
use super::CancellationToken;
use super::Deserialize;
use super::HttpRequest;
#[cfg(test)]
use super::HttpTransport;
use super::ModelCapabilities;
use super::ModelError;
use super::ModelHttp;
use super::ModelRequest;
use super::ModelResult;
use super::ModelRuntimeConfig;
use super::ModelStream;
use super::PreparedModelCall;
use super::PreparedModelRequest;
use super::ProtocolErrorKind;
use super::RequestAuth;
use super::RequestCredential;
use super::Serialize;
use super::Url;
use std::fmt::{Debug, Formatter};

pub(crate) const PROVIDER_NAME: &str = "openai_responses";

pub(crate) const TOOL_NAMESPACE: &str = "peri";

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
    pub(super) endpoint: Url,
    pub(super) bearer: String,
    pub(super) auth: Option<Arc<dyn RequestAuth>>,
    pub(super) model: String,
    pub(super) profile: ResponsesProfile,
    pub(super) credential_ref: String,
    pub(super) reasoning_effort: Option<String>,
    pub(super) max_tokens: Option<u32>,
    pub(super) context_window: Option<u32>,
    pub(super) runtime: ModelRuntimeConfig,
}

impl ResponsesConfig {
    #[must_use]
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

    #[must_use]
    pub fn with_profile(mut self, profile: ResponsesProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Stable account/grant identity, never the access or refresh token itself.
    #[must_use]
    pub fn with_credential_ref(mut self, credential_ref: impl Into<String>) -> Self {
        if self.auth.is_none() {
            self.credential_ref = credential_ref.into();
        }
        self
    }

    #[must_use]
    pub fn with_auth(mut self, auth: Arc<dyn RequestAuth>) -> Self {
        self.credential_ref = auth.credential_ref().to_owned();
        self.auth = Some(auth);
        self.bearer.clear();
        self
    }

    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    #[must_use]
    pub fn with_context_window(mut self, context_window: Option<u32>) -> Self {
        self.context_window = context_window;
        self
    }

    #[must_use]
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
    pub(super) config: ResponsesConfig,
    pub(super) http: ModelHttp,
}

impl Debug for ResponsesModel {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponsesModel")
            .finish_non_exhaustive()
    }
}

impl ResponsesModel {
    /// client 构造失败（如平台 TLS 根证书不可用）保留到首次调用返回，构造期不 panic。
    #[must_use]
    pub fn new(config: ResponsesConfig) -> Self {
        // In particular, never forward a ChatGPT grant to a redirect target.
        let http = ModelHttp::for_runtime(&config.runtime, false);
        Self { config, http }
    }

    #[cfg(test)]
    pub(super) fn with_transport(
        config: ResponsesConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            config,
            http: ModelHttp::with_transport(transport),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_client_failure(config: ResponsesConfig, error: ModelError) -> Self {
        Self {
            config,
            http: ModelHttp::unavailable(error),
        }
    }

    pub(super) fn native_request(
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
        let (client, transport) = self.http.client_and_transport()?;
        let call_context = request.call_context.clone();
        let built = Arc::new(request::build_request(&self.config, &request)?);
        let checkpoint = serde_json::json!({
            "provider": PROVIDER_NAME,
            "endpoint": built.endpoint.as_str(),
            "model": self.config.model,
            "credentialRef": self.config.credential_ref,
            "body": built.body,
        });
        let runtime = self.config.runtime.clone();
        let bearer = self.config.bearer.clone();
        let decoders = stream::decoders(built.binding.clone(), built.tool_names.clone());
        if let Some(auth) = self.config.auth.clone() {
            let endpoint_auth = Arc::clone(&auth);
            let request_factory = Arc::new(move |credential: &RequestCredential| {
                endpoint_auth.validate_endpoint(&built.endpoint)?;
                Self::native_request(&client, credential.bearer(), &built)
                    .map(|wire| wire.with_call_context(call_context.clone()))
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
        let request_factory = Arc::new(move || {
            Self::native_request(&client, &bearer, &built)
                .map(|wire| wire.with_call_context(call_context.clone()))
        });
        Ok(PreparedModelCall::new(checkpoint, move |cancellation| {
            Ok(super::runtime_http_sse_stream(
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
