use super::async_trait;
use super::fmt;
use super::request;
use super::stream;
use super::Arc;
use super::BuiltAnthropicRequest;
use super::CancellationToken;
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
use super::PreparedModelRequest;
use super::Url;
use std::fmt::{Debug, Formatter};

pub(super) const PROVIDER_NAME: &str = "anthropic";

pub(super) const DEFAULT_MAX_TOKENS: u32 = 32_000;

/// Anthropic Messages API 的强类型配置。
///
/// 认证凭据仅保存在此配置和模型内部；其 `Debug` 实现永不输出凭据。
pub struct AnthropicConfig {
    pub(super) endpoint: Url,
    pub(super) api_key: String,
    pub(super) model: String,
    pub(super) extended_thinking: bool,
    pub(super) thinking_budget: u32,
    pub(super) thinking_effort: String,
    pub(super) enable_cache: bool,
    pub(super) max_tokens: u32,
    pub(super) context_window: Option<u32>,
    pub(super) runtime: ModelRuntimeConfig,
}

impl AnthropicConfig {
    /// 显式创建配置；环境变量解析属于上层应用，不在协议 crate 中提供。
    #[must_use]
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
            context_window: None,
            runtime: ModelRuntimeConfig::default(),
        }
    }

    /// 启用 thinking 并原样传递 effort；精确模型 `claude-sonnet-5-5` 使用 adaptive，
    /// 忽略手动预算，其余模型保留手动预算的收缩与省略规则（最后一个 assistant turn
    /// 没有带签名 thinking 块时也不发送）。
    #[must_use]
    pub fn with_extended_thinking(mut self, budget_tokens: u32, effort: impl Into<String>) -> Self {
        self.extended_thinking = true;
        self.thinking_budget = budget_tokens;
        self.thinking_effort = effort.into();
        self
    }

    #[must_use]
    pub fn without_cache(mut self) -> Self {
        self.enable_cache = false;
        self
    }

    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// 写入装配处已解析的上下文窗口；不传即 `None`（未知，不代填）。
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

impl fmt::Debug for AnthropicConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicConfig")
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key)
            .field("model", &self.model)
            .field("extended_thinking", &self.extended_thinking)
            .field("thinking_budget", &self.thinking_budget)
            .field("thinking_effort", &self.thinking_effort)
            .field("enable_cache", &self.enable_cache)
            .field("max_tokens", &self.max_tokens)
            .field("context_window", &self.context_window)
            .field("runtime", &self.runtime)
            .finish()
    }
}

/// Anthropic Messages API 模型。
pub struct AnthropicModel {
    pub(super) config: AnthropicConfig,
    pub(super) http: ModelHttp,
}

impl Debug for AnthropicModel {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicModel")
            .finish_non_exhaustive()
    }
}

impl AnthropicModel {
    /// 从显式、强类型配置创建模型。
    ///
    /// transport 与 native request path 共享同一个 `reqwest::Client`（clone 仅
    /// 增加引用计数，连接池 / TLS session cache 复用），不再创建双 client。
    /// client 构造失败（如平台 TLS 根证书不可用）保留到首次调用返回，构造期不 panic。
    #[must_use]
    pub fn new(config: AnthropicConfig) -> Self {
        let http = ModelHttp::for_runtime(&config.runtime, true);
        Self { config, http }
    }

    #[cfg(test)]
    pub(crate) fn with_transport(
        config: AnthropicConfig,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            config,
            http: ModelHttp::with_transport(transport),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_client_failure(config: AnthropicConfig, error: ModelError) -> Self {
        Self {
            config,
            http: ModelHttp::unavailable(error),
        }
    }

    /// 所有 public request path 共用的私有构造结果。
    pub(super) fn build_request(
        &self,
        request: &ModelRequest,
    ) -> ModelResult<BuiltAnthropicRequest> {
        request::build_request(&self.config, request)
    }

    pub(super) fn native_http_request(
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
        request.build().map(HttpRequest::new).map_err(|error| {
            ModelError::protocol(crate::ProtocolErrorKind::Provider).with_error(&error)
        })
    }
}

#[async_trait]
impl crate::Model for AnthropicModel {
    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<crate::PreparedModelCall> {
        let (client, transport) = self.http.client_and_transport()?;
        let call_context = request.call_context.clone();
        let built = Arc::new(self.build_request(&request)?);
        let checkpoint =
            crate::protocol::prepared::checkpoint(PROVIDER_NAME, &built.endpoint, &built.body)?;
        let api_key = self.config.api_key.clone();
        let cache_enabled = self.config.enable_cache;
        let request_factory = Arc::new(move || {
            Self::native_http_request(&client, &api_key, cache_enabled, &built)
                .map(|wire| wire.with_call_context(call_context.clone()))
        });
        let runtime = self.config.runtime.clone();
        Ok(crate::PreparedModelCall::new(
            checkpoint,
            move |cancellation| {
                Ok(super::runtime_http_sse_stream(
                    &runtime,
                    cancellation,
                    transport,
                    request_factory,
                    Arc::<str>::from(PROVIDER_NAME),
                    stream::decoders(),
                ))
            },
        ))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_reasoning: true,
            supports_vision: true,
            supports_streaming: true,
        }
    }

    /// provider 已解析的单次输出上限（`config.max_tokens`）。摘要器等派生请求
    /// 只在两个来源一致时被允许沿用，不借助请求参数覆写它（H6）。
    fn output_token_limit(&self) -> Option<u32> {
        Some(self.config.max_tokens)
    }

    /// 装配处写入的已解析上下文窗口；未写入即未知。
    fn context_window(&self) -> Option<u32> {
        self.config.context_window
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
        let (client, transport) = self.http.client_and_transport()?;
        let call_context = request.call_context.clone();
        let built = Arc::new(self.build_request(&request)?);
        let api_key = self.config.api_key.clone();
        let cache_enabled = self.config.enable_cache;
        let request_factory = {
            let built = Arc::clone(&built);
            Arc::new(move || {
                Self::native_http_request(&client, &api_key, cache_enabled, &built)
                    .map(|wire| wire.with_call_context(call_context.clone()))
            })
        };
        Ok(super::runtime_http_sse_stream(
            &self.config.runtime,
            cancellation,
            transport,
            request_factory,
            Arc::<str>::from(PROVIDER_NAME),
            stream::decoders(),
        ))
    }
}
