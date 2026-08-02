//! Anthropic Messages API adapter。
//!
//! 本模块只产生和消费标准 `peri-model` 协议；不会引用 Agent 事件或类型。

mod cache;
mod request;
mod response;
mod stream;

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    runtime::stream::runtime_http_sse_stream,
    transport::{HttpRequest, HttpTransport, ReqwestTransport},
    ModelCapabilities, ModelError, ModelRequest, ModelResult, ModelRuntimeConfig, ModelStream,
    PreparedModelRequest,
};

use request::BuiltAnthropicRequest;

const PROVIDER_NAME: &str = "anthropic";
const DEFAULT_MAX_TOKENS: u32 = 32_000;

/// Anthropic Messages API 的强类型配置。
///
/// 认证凭据仅保存在此配置和模型内部；其 `Debug` 实现永不输出凭据。
pub struct AnthropicConfig {
    endpoint: Url,
    api_key: String,
    model: String,
    extended_thinking: bool,
    thinking_budget: u32,
    thinking_effort: String,
    enable_cache: bool,
    max_tokens: u32,
    runtime: ModelRuntimeConfig,
}

impl AnthropicConfig {
    /// 显式创建配置；环境变量解析属于上层应用，不在协议 crate 中提供。
    pub fn new(endpoint: Url, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint,
            api_key: api_key.into(),
            model: model.into(),
            extended_thinking: false,
            thinking_budget: 10_000,
            thinking_effort: "medium".into(),
            enable_cache: true,
            max_tokens: DEFAULT_MAX_TOKENS,
            runtime: ModelRuntimeConfig::default(),
        }
    }

    pub fn with_extended_thinking(mut self, budget_tokens: u32, effort: impl Into<String>) -> Self {
        self.extended_thinking = true;
        self.thinking_budget = budget_tokens;
        self.thinking_effort = effort.into();
        self
    }

    pub fn without_cache(mut self) -> Self {
        self.enable_cache = false;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_runtime(mut self, runtime: ModelRuntimeConfig) -> Self {
        self.runtime = runtime;
        self
    }
}

impl fmt::Debug for AnthropicConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicConfig")
            .field("endpoint", &debug_endpoint_projection(&self.endpoint))
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("extended_thinking", &self.extended_thinking)
            .field("thinking_budget", &self.thinking_budget)
            .field("thinking_effort", &self.thinking_effort)
            .field("enable_cache", &self.enable_cache)
            .field("max_tokens", &self.max_tokens)
            .field("runtime", &self.runtime)
            .finish()
    }
}

fn debug_endpoint_projection(endpoint: &Url) -> String {
    match endpoint.host() {
        Some(host) => format!("{}://{host}/[REDACTED]", endpoint.scheme()),
        None => format!("{}://[REDACTED]", endpoint.scheme()),
    }
}

/// Anthropic Messages API 模型。
pub struct AnthropicModel {
    config: AnthropicConfig,
    transport: Arc<dyn HttpTransport>,
    client: reqwest::Client,
}

impl AnthropicModel {
    /// 从显式、强类型配置创建模型。
    ///
    /// transport 与 native request path 共享同一个 `reqwest::Client`（clone 仅
    /// 增加引用计数，连接池 / TLS session cache 复用），不再创建双 client。
    pub fn new(config: AnthropicConfig) -> Self {
        let client = reqwest::Client::new();
        Self {
            config,
            transport: Arc::new(ReqwestTransport::new(client.clone())),
            client,
        }
    }

    #[cfg(test)]
    fn with_transport(config: AnthropicConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            config,
            transport,
            client: reqwest::Client::new(),
        }
    }

    /// 所有 public request path 共用的私有构造结果。
    fn build_request(&self, request: &ModelRequest) -> ModelResult<BuiltAnthropicRequest> {
        request::build_request(&self.config, request)
    }

    fn native_http_request(
        client: &reqwest::Client,
        api_key: &str,
        cache_enabled: bool,
        built: &BuiltAnthropicRequest,
    ) -> ModelResult<HttpRequest> {
        let mut request = client
            .post(built.endpoint.clone())
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&built.body);
        if cache_enabled {
            request = request.header("anthropic-beta", "prompt-caching-2024-07-31");
        }
        if let Some(session_id) = &built.session_id {
            request = request.header("x-session-id", session_id);
        }
        request
            .build()
            .map(HttpRequest::new)
            .map_err(|_| ModelError::protocol(crate::ProtocolErrorKind::Provider))
    }
}

#[async_trait]
impl crate::Model for AnthropicModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_reasoning: true,
            supports_vision: true,
            supports_streaming: true,
        }
    }

    fn prepare_request(&self, request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        self.build_request(request)?.observe(&self.config.runtime)
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        if cancellation.is_cancelled() {
            return Err(ModelError::cancelled());
        }
        let built = Arc::new(self.build_request(&request)?);
        let client = self.client.clone();
        let api_key = self.config.api_key.clone();
        let cache_enabled = self.config.enable_cache;
        let request_factory = {
            let built = Arc::clone(&built);
            Arc::new(move || Self::native_http_request(&client, &api_key, cache_enabled, &built))
        };
        Ok(runtime_http_sse_stream(
            &self.config.runtime,
            cancellation,
            Arc::clone(&self.transport),
            request_factory,
            Arc::<str>::from(PROVIDER_NAME),
            stream::decoders(),
        ))
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod mod_test;
