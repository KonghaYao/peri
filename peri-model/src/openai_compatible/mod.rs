//! OpenAI-compatible Chat Completions provider。
//!
//! 本模块只产生和消费标准 `peri-model` 协议；不会引用 Agent 事件或类型。

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

use request::BuiltOpenAiRequest;

const PROVIDER_NAME: &str = "openai-compatible";
const DEFAULT_MAX_TOKENS: u32 = 32_000;

/// OpenAI-compatible Chat Completions 的强类型配置。
///
/// 认证凭据仅保存在此配置和模型内部；其 `Debug` 实现永不输出凭据。
pub struct OpenAiConfig {
    endpoint: Url,
    api_key: String,
    model: String,
    reasoning_effort: Option<String>,
    thinking_enabled: bool,
    supports_thinking_content: bool,
    max_tokens: u32,
    runtime: ModelRuntimeConfig,
}

impl OpenAiConfig {
    /// 显式创建配置；环境变量解析属于上层应用，不在协议 crate 中提供。
    pub fn new(endpoint: Url, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint,
            api_key: api_key.into(),
            model: model.into(),
            reasoning_effort: None,
            thinking_enabled: false,
            supports_thinking_content: false,
            max_tokens: DEFAULT_MAX_TOKENS,
            runtime: ModelRuntimeConfig::default(),
        }
    }

    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    pub fn with_thinking_enabled(mut self, enabled: bool) -> Self {
        self.thinking_enabled = enabled;
        self
    }

    pub fn with_thinking_content(mut self, enabled: bool) -> Self {
        self.supports_thinking_content = enabled;
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

impl fmt::Debug for OpenAiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiConfig")
            .field("endpoint", &debug_endpoint_projection(&self.endpoint))
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("thinking_enabled", &self.thinking_enabled)
            .field("supports_thinking_content", &self.supports_thinking_content)
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

/// OpenAI-compatible Chat Completions 模型。
pub struct OpenAiModel {
    config: OpenAiConfig,
    transport: Arc<dyn HttpTransport>,
    client: reqwest::Client,
}

impl OpenAiModel {
    /// 从显式、强类型配置创建模型。
    ///
    /// transport 与 native request path 共享同一个 `reqwest::Client`（clone 仅
    /// 增加引用计数，连接池 / TLS session cache 复用），不再创建双 client。
    pub fn new(config: OpenAiConfig) -> Self {
        let client = reqwest::Client::new();
        Self {
            config,
            transport: Arc::new(ReqwestTransport::new(client.clone())),
            client,
        }
    }

    #[cfg(test)]
    fn with_transport(config: OpenAiConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            config,
            transport,
            client: reqwest::Client::new(),
        }
    }

    /// 所有 public request path 共用的私有构造结果。
    fn build_request(&self, request: &ModelRequest) -> ModelResult<BuiltOpenAiRequest> {
        request::build_request(&self.config, request)
    }

    fn native_http_request(
        client: &reqwest::Client,
        api_key: &str,
        built: &BuiltOpenAiRequest,
    ) -> ModelResult<HttpRequest> {
        let request = client
            .post(built.endpoint.clone())
            .bearer_auth(api_key)
            .json(&built.body)
            .build()
            .map_err(|_| ModelError::protocol(crate::ProtocolErrorKind::Provider))?;
        Ok(HttpRequest::new(request))
    }
}

#[async_trait]
impl crate::Model for OpenAiModel {
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
        let request_factory = {
            let built = Arc::clone(&built);
            Arc::new(move || Self::native_http_request(&client, &api_key, &built))
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
