use async_trait::async_trait;
use serde_json::Value;

use crate::{
    error::{AgentError, AgentResult},
    llm::types::{LlmRequest, LlmResponse, StopReason, TokenUsage},
    messages::{BaseMessage, ContentBlock, MessageContent, ToolCallRequest},
    tools::ToolDefinition,
};

/// ProviderAdapter — 封装 Provider 特定的数据转换差异。
///
/// 职责边界：ContentBlock ↔ Provider JSON 的双向转换 + HTTP 传输配置。
/// 不负责 System 消息的提取——该逻辑留在 build_request_body 中由各 adapter 自行处理。
///
/// GenericInvoker 是瞬态辅助器，不持有 adapter 引用。
/// ChatAnthropic/ChatOpenAI 直接持有 adapter + client。
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    // ─── 标识 ───
    fn provider_name(&self) -> &str;
    fn model_id(&self) -> &str;
    fn context_window(&self) -> u32 {
        200_000
    }

    // ─── 1. 消息序列化（System 不进此层，由 build_request_body 自行处理） ───
    /// 将 BaseMessage 列表（不含 System 消息）序列化为 Provider 格式的 JSON 数组。
    /// System 消息的提取和放置由 build_request_body 处理——不同 Provider 差异太大。
    fn serialize_messages(&self, messages: &[BaseMessage]) -> Vec<Value>;

    /// 将单个 ContentBlock 序列化为 Provider 格式的 JSON fragment。
    /// 目前仅通过静态分发调用（adapter 内部 `block_to_xxx_part` 函数），
    /// trait 方法入口保留供未来 streaming 路径中动态分发工具结果回放等场景。
    fn serialize_content_block(&self, block: &ContentBlock) -> Option<Value>;

    // ─── 2. 请求体构建 ───
    /// 将 ToolDefinition 序列化为 Provider 特定的 tool JSON。
    fn serialize_tool(&self, tool: &ToolDefinition) -> Value;

    /// 构建完整的 HTTP 请求体。
    /// 内部自行处理：System 消息提取+放置、thinking 配置、cache 标记等。
    fn build_request_body(&self, request: &LlmRequest, streaming: bool) -> Value;

    // ─── 3. 响应解析 ───
    /// 从响应 JSON 无条件解析所有 ContentBlock + ToolCallRequest。
    /// 必须不依赖 stop_reason 做分支——stop_reason 由 GenericInvoker 单独提取后决定使用方式。
    fn parse_response_content(
        &self,
        response_json: &Value,
    ) -> AgentResult<(Vec<ContentBlock>, Vec<ToolCallRequest>)>;

    /// 从响应 JSON 提取 StopReason。
    fn extract_stop_reason(&self, response_json: &Value) -> StopReason;

    /// 从响应 JSON 提取 TokenUsage。
    fn extract_usage(
        &self,
        response_json: &Value,
        request_id: Option<String>,
    ) -> Option<TokenUsage>;

    /// 从错误响应 JSON 提取人类可读错误消息。
    fn extract_error_message(&self, response_json: &Value) -> String;

    /// 从错误响应 JSON 提取错误类型（如 "overloaded_error"、"invalid_request_error"）。
    /// 默认返回 None（OpenAI 从 details 中提取，非 JSON body 直接字段）。
    fn extract_error_type(&self, _response_json: &Value) -> Option<String> {
        None
    }

    /// 从 HTTP response headers 优先提取 request_id，
    /// 由 GenericInvoker 先调用，若返回 None 则 fallback 到 extract_request_id_from_body。
    fn extract_request_id_from_headers(
        &self,
        headers: &reqwest::header::HeaderMap,
    ) -> Option<String>;

    /// 从响应 JSON body 提取 request_id（作为 headers fallback）。
    fn extract_request_id_from_body(&self, response_json: &Value) -> Option<String>;

    // ─── 4. 日志 ───
    /// 在 GenericInvoker 的成功日志之后，输出 Provider 特有字段（如 cache 指标）。
    /// 默认空实现——OpenAI 不需要额外日志。
    fn log_success_extra(&self, response_json: &Value, elapsed_ms: u64, msg_count: usize) {
        let _ = (response_json, elapsed_ms, msg_count);
    }

    /// 在 GenericInvoker 的错误处理中，输出 Provider 特有错误上下文。
    /// Anthropic 覆盖此方法以在 500 错误时 dump 请求体。
    /// body: 请求体引用（用于 dump），仅在非流式路径可用。
    fn log_error_extra(&self, status: u16, response_json: &Value, body: Option<&Value>) {
        let _ = (status, response_json, body);
    }

    // ─── 5. HTTP 传输配置 ───
    /// 构建 API endpoint URL。
    fn build_chat_url(&self) -> String;

    /// 在 reqwest 请求上添加 Provider 特定的 headers（auth、version、beta、session-id 等）。
    fn apply_auth_headers(
        &self,
        req: reqwest::RequestBuilder,
        session_id: Option<&str>,
    ) -> reqwest::RequestBuilder;
}

/// GenericInvoker — 无状态的共享非流式 HTTP 调用辅助器。
///
/// 设计变更：不再持有 adapter/client。invoke() 以借用方式接收 &A 和 &Client。
/// ChatAnthropic/ChatOpenAI 直接持有自己的 adapter + client，无双重实例问题。
pub struct GenericInvoker;

impl GenericInvoker {
    /// 非流式 invoke 的完整模板方法。
    ///
    /// 流程：
    /// 1. build_request_body → 2. apply_auth_headers + send → 3. 状态检查 + 错误提取 →
    /// 4. parse_response_content → 5. 成功日志 → 6. build_base_message → 7. 返回 LlmResponse
    pub async fn invoke<A: ProviderAdapter>(
        adapter: &A,
        client: &reqwest::Client,
        request: LlmRequest,
    ) -> AgentResult<LlmResponse> {
        let msg_count = request.messages.len();
        let start = std::time::Instant::now();

        // 1. 构建请求体
        let body = adapter.build_request_body(&request, false);

        // 2. 发送 HTTP 请求
        let session_id = request.session_id.as_deref();
        let req = adapter.apply_auth_headers(client.post(adapter.build_chat_url()), session_id);
        let resp = req
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(
                    provider = adapter.provider_name(),
                    model = adapter.model_id(),
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    error = %e,
                    "LLM 网络请求失败"
                );
                AgentError::LlmError(e.to_string())
            })?;

        let status = resp.status();
        // 在消费 resp body 前保存 headers
        let resp_headers = resp.headers().clone();

        let resp_text = resp.text().await.map_err(|e| {
            tracing::error!(
                provider = adapter.provider_name(),
                model = adapter.model_id(),
                status = %status,
                elapsed_ms = start.elapsed().as_millis() as u64,
                error = %e,
                "LLM 读取响应体失败"
            );
            AgentError::LlmError(format!("读取响应体失败: {e}"))
        })?;
        let resp_json: Value = serde_json::from_str(&resp_text).map_err(|e| {
            tracing::error!(
                provider = adapter.provider_name(),
                model = adapter.model_id(),
                status = %status,
                elapsed_ms = start.elapsed().as_millis() as u64,
                error = %e,
                "LLM 响应解析失败"
            );
            AgentError::LlmError(format!(
                "解析响应失败: {e}\n原始响应({status}): {resp_text}"
            ))
        })?;

        // 3. 非成功状态 → 错误处理
        if !status.is_success() {
            let error_msg = adapter.extract_error_message(&resp_json);
            let error_type = adapter.extract_error_type(&resp_json);
            adapter.log_error_extra(status.as_u16(), &resp_json, Some(&body));

            tracing::error!(
                provider = adapter.provider_name(),
                model = adapter.model_id(),
                status = %status,
                error_type = error_type.unwrap_or_else(|| "unknown".to_string()),
                error_message = %error_msg,
                elapsed_ms = start.elapsed().as_millis() as u64,
                msg_count,
                "LLM API 错误"
            );
            return Err(AgentError::LlmHttpError {
                status: status.as_u16(),
                message: format!("API 错误 {status}: {error_msg}"),
            });
        }

        // 4. 解析成功响应
        let stop_reason = adapter.extract_stop_reason(&resp_json);

        // 先提取 request_id（headers 优先 + body fallback）
        let request_id = adapter
            .extract_request_id_from_headers(&resp_headers)
            .or_else(|| adapter.extract_request_id_from_body(&resp_json));

        let (blocks, tool_calls) = adapter.parse_response_content(&resp_json)?;
        let usage = adapter.extract_usage(&resp_json, request_id.clone());

        // 5. 通用成功日志
        tracing::info!(
            provider = adapter.provider_name(),
            model = adapter.model_id(),
            status = %status,
            elapsed_ms = start.elapsed().as_millis() as u64,
            msg_count,
            input_tokens = usage.as_ref().map(|u| u.input_tokens).unwrap_or(0),
            output_tokens = usage.as_ref().map(|u| u.output_tokens).unwrap_or(0),
            "LLM invoke completed"
        );
        // Provider 特有日志字段（如 Anthropic cache 指标）
        adapter.log_success_extra(&resp_json, start.elapsed().as_millis() as u64, msg_count);

        // 6. 构造 BaseMessage（两个 Provider 完全相同的逻辑）
        let message = Self::build_base_message(blocks, tool_calls, &stop_reason);

        Ok(LlmResponse {
            message,
            stop_reason,
            usage,
            request_id,
        })
    }

    /// (blocks, tool_calls) → BaseMessage（跨 Provider 共享逻辑）
    /// 也从 Anthropic streaming 路径调用（避免重复实现）。
    pub(crate) fn build_base_message(
        blocks: Vec<ContentBlock>,
        tool_calls: Vec<ToolCallRequest>,
        stop_reason: &StopReason,
    ) -> BaseMessage {
        let _ = stop_reason; // 已由调用方通过 stop_reason 做前置判别
        if !tool_calls.is_empty() {
            let content = if let [single] = blocks.as_slice() {
                if let Some(text) = single.as_text() {
                    MessageContent::text(text)
                } else {
                    MessageContent::Blocks(blocks)
                }
            } else {
                MessageContent::Blocks(blocks)
            };
            BaseMessage::ai_with_tool_calls(content, tool_calls)
        } else if let [single] = blocks.as_slice() {
            if let Some(text) = single.as_text() {
                BaseMessage::ai(text)
            } else {
                BaseMessage::ai(MessageContent::Blocks(blocks))
            }
        } else if blocks.is_empty() {
            BaseMessage::ai("")
        } else {
            BaseMessage::ai(MessageContent::Blocks(blocks))
        }
    }
}
