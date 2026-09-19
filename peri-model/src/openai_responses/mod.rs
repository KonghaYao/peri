//! OpenAI Responses API provider。
//!
//! 本模块只产生和消费标准 `peri-model` 协议；不会引用 Agent 事件或类型。与
//! `openai_compatible` 的差异在于 Responses 的 input 是扁平 item 序列，且
//! reasoning/message/function_call 原生 item 必须逐项保真回放：adapter 把
//! `response.completed` 的 output 记录进 [`ResponsesHistoryV1`]，并在下一轮按实际
//! endpoint/model 校验后回放（见 `protocol::ContentBlock::ResponsesNativeHistory`）。

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

use request::BuiltResponsesRequest;

const PROVIDER_NAME: &str = "openai-responses";
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 32_000;

/// OpenAI Responses API 的强类型配置。
///
/// 认证凭据仅保存在此配置和模型内部；其 `Debug` 实现永不输出凭据。
pub struct OpenAiResponsesConfig {
    endpoint: Url,
    api_key: String,
    model: String,
    reasoning_effort: Option<String>,
    max_output_tokens: u32,
    runtime: ModelRuntimeConfig,
}

impl OpenAiResponsesConfig {
    /// 显式创建配置；环境变量解析属于上层应用，不在协议 crate 中提供。
    ///
    /// `endpoint` 是 provider base URL（例如 `https://api.openai.com/v1/`），
    /// adapter 在其上构造 `responses` 路径。
    pub fn new(endpoint: Url, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint,
            api_key: api_key.into(),
            model: model.into(),
            reasoning_effort: None,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            runtime: ModelRuntimeConfig::default(),
        }
    }

    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    pub fn with_max_output_tokens(mut self, max_output_tokens: u32) -> Self {
        self.max_output_tokens = max_output_tokens;
        self
    }

    pub fn with_runtime(mut self, runtime: ModelRuntimeConfig) -> Self {
        self.runtime = runtime;
        self
    }
}

impl fmt::Debug for OpenAiResponsesConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiResponsesConfig")
            .field("endpoint", &debug_endpoint_projection(&self.endpoint))
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("max_output_tokens", &self.max_output_tokens)
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

/// OpenAI Responses API 模型。
pub struct OpenAiResponsesModel {
    config: OpenAiResponsesConfig,
    transport: Arc<dyn HttpTransport>,
    client: reqwest::Client,
}

impl OpenAiResponsesModel {
    /// 从显式、强类型配置创建模型。
    ///
    /// transport 与 native request path 共享同一个 `reqwest::Client`（clone 仅增加
    /// 引用计数，连接池 / TLS session cache 复用）。
    pub fn new(config: OpenAiResponsesConfig) -> Self {
        let client = reqwest::Client::new();
        Self {
            config,
            transport: Arc::new(ReqwestTransport::new(client.clone())),
            client,
        }
    }

    #[cfg(test)]
    fn with_transport(config: OpenAiResponsesConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            config,
            transport,
            client: reqwest::Client::new(),
        }
    }

    /// 所有 public request path 共用的私有构造结果。
    fn build_request(&self, request: &ModelRequest) -> ModelResult<BuiltResponsesRequest> {
        request::build_request(&self.config, request)
    }

    fn native_http_request(
        client: &reqwest::Client,
        api_key: &str,
        built: &BuiltResponsesRequest,
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
impl crate::Model for OpenAiResponsesModel {
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
            stream::decoders(built.endpoint.clone(), built.model_id.clone()),
        ))
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod mod_test;
