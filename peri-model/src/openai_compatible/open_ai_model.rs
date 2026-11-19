use super::async_trait;
use super::fmt;
use super::request;
use super::stream;
use super::Arc;
use super::BuiltOpenAiRequest;
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

pub(super) const PROVIDER_NAME: &str = "openai-compatible";

pub(super) const DEFAULT_MAX_TOKENS: u32 = 32_000;

/// OpenAI-compatible Chat Completions 的强类型配置。
///
/// 认证凭据仅保存在此配置和模型内部；其 `Debug` 实现永不输出凭据。
pub struct OpenAiConfig {
    pub(super) endpoint: Url,
    pub(super) api_key: String,
    pub(super) model: String,
    pub(super) reasoning_effort: Option<String>,
    pub(super) thinking_enabled: bool,
    pub(super) supports_thinking_content: bool,
    pub(super) max_tokens: u32,
    pub(super) context_window: Option<u32>,
    pub(super) runtime: ModelRuntimeConfig,
}

impl OpenAiConfig {
    /// 显式创建配置；环境变量解析属于上层应用，不在协议 crate 中提供。
    #[must_use]
    pub fn new(endpoint: Url, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint,
            api_key: api_key.into(),
            model: model.into(),
            reasoning_effort: None,
            thinking_enabled: false,
            supports_thinking_content: false,
            max_tokens: DEFAULT_MAX_TOKENS,
            context_window: None,
            runtime: ModelRuntimeConfig::default(),
        }
    }

    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    #[must_use]
    pub fn with_thinking_enabled(mut self, enabled: bool) -> Self {
        self.thinking_enabled = enabled;
        self
    }

    #[must_use]
    pub fn with_thinking_content(mut self, enabled: bool) -> Self {
        self.supports_thinking_content = enabled;
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

impl fmt::Debug for OpenAiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiConfig")
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key)
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("thinking_enabled", &self.thinking_enabled)
            .field("supports_thinking_content", &self.supports_thinking_content)
            .field("max_tokens", &self.max_tokens)
            .field("context_window", &self.context_window)
            .field("runtime", &self.runtime)
            .finish()
    }
}

/// OpenAI-compatible Chat Completions 模型。
pub struct OpenAiModel {
    pub(super) config: OpenAiConfig,
    pub(super) http: ModelHttp,
}

impl Debug for OpenAiModel {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiModel")
            .finish_non_exhaustive()
    }
}

impl OpenAiModel {
    /// 从显式、强类型配置创建模型。
    ///
    /// transport 与 native request path 共享同一个 `reqwest::Client`（clone 仅
    /// 增加引用计数，连接池 / TLS session cache 复用），不再创建双 client。
    /// client 构造失败（如平台 TLS 根证书不可用）保留到首次调用返回，构造期不 panic。
    #[must_use]
    pub fn new(config: OpenAiConfig) -> Self {
        let http = ModelHttp::for_runtime(&config.runtime, true);
        Self { config, http }
    }

    #[cfg(test)]
    pub(crate) fn with_transport(config: OpenAiConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            config,
            http: ModelHttp::with_transport(transport),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_client_failure(config: OpenAiConfig, error: ModelError) -> Self {
        Self {
            config,
            http: ModelHttp::unavailable(error),
        }
    }

    /// 所有 public request path 共用的私有构造结果。
    pub(super) fn build_request(&self, request: &ModelRequest) -> ModelResult<BuiltOpenAiRequest> {
        request::build_request(&self.config, request)
    }

    pub(super) fn native_http_request(
        client: &reqwest::Client,
        api_key: &str,
        built: &BuiltOpenAiRequest,
    ) -> ModelResult<HttpRequest> {
        let request = client
            .post(built.endpoint.clone())
            .bearer_auth(api_key)
            .json(&built.body)
            .build()
            .map_err(|error| {
                ModelError::protocol(crate::ProtocolErrorKind::Provider).with_error(&error)
            })?;
        Ok(HttpRequest::new(request))
    }
}

#[async_trait]
impl crate::Model for OpenAiModel {
    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<crate::PreparedModelCall> {
        let (client, transport) = self.http.client_and_transport()?;
        let call_context = request.call_context.clone();
        let built = Arc::new(self.build_request(&request)?);
        let checkpoint =
            crate::protocol::prepared::checkpoint(PROVIDER_NAME, &built.endpoint, &built.body)?;
        let api_key = self.config.api_key.clone();
        let request_factory = Arc::new(move || {
            Self::native_http_request(&client, &api_key, &built)
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
        let request_factory = {
            let built = Arc::clone(&built);
            Arc::new(move || {
                Self::native_http_request(&client, &api_key, &built)
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
