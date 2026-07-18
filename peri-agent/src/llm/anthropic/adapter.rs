use async_trait::async_trait;
use serde_json::{json, Value};

use super::cache::{self, SystemPromptBlock, SYSTEM_PROMPT_DYNAMIC_BOUNDARY};
use crate::{
    error::{AgentError, AgentResult},
    llm::provider_adapter::ProviderAdapter,
    llm::types::{LlmRequest, StopReason, TokenUsage},
    messages::{BaseMessage, ContentBlock, ImageSource, MessageContent, ToolCallRequest},
    tools::ToolDefinition,
};

/// AnthropicAdapter — 所有 Anthropic Provider 特定逻辑
pub(super) struct AnthropicAdapter {
    pub(super) api_key: String,
    pub(super) model: String,
    pub(super) base_url: Option<String>,
    pub(super) extended_thinking: bool,
    pub(super) thinking_budget: u32,
    pub(super) thinking_effort: String,
    pub(super) enable_cache: bool,
    pub(super) max_tokens: u32,
}

// ─── ContentBlock → Anthropic content part ────────────────────────────────

fn block_to_anthropic(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::Image { source } => match source {
            ImageSource::Base64 { media_type, data } => Some(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data
                }
            })),
            ImageSource::Url { url } => Some(json!({
                "type": "image",
                "source": { "type": "url", "url": url }
            })),
        },
        ContentBlock::Document { source, title } => {
            let src = serde_json::to_value(source).unwrap_or_default();
            let mut obj = json!({ "type": "document", "source": src });
            if let Some(t) = title {
                obj["title"] = json!(t);
            }
            Some(obj)
        }
        ContentBlock::ToolUse { id, name, input } => Some(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input
        })),
        ContentBlock::ToolResult {
            id,
            tool_use_id,
            content,
            is_error,
        } => {
            let content_val: Vec<Value> = content.iter().filter_map(block_to_anthropic).collect();
            let block_id = id
                .clone()
                .unwrap_or_else(|| format!("toolu_{}", uuid::Uuid::now_v7()));
            Some(json!({
                "type": "tool_result",
                "id": block_id,
                "tool_use_id": tool_use_id,
                "content": content_val,
                "is_error": is_error
            }))
        }
        ContentBlock::Reasoning { text, signature } => {
            let mut obj = json!({ "type": "thinking", "thinking": text });
            if let Some(sig) = signature {
                obj["signature"] = json!(sig);
            }
            Some(obj)
        }
        ContentBlock::Unknown(v) => Some(v.clone()),
    }
}

fn content_to_anthropic(content: &MessageContent) -> Value {
    match content {
        MessageContent::Text(s) => json!([{"type": "text", "text": s}]),
        MessageContent::Blocks(blocks) => {
            let parts: Vec<Value> = blocks.iter().filter_map(block_to_anthropic).collect();
            Value::Array(parts)
        }
        MessageContent::Raw(values) => Value::Array(values.clone()),
    }
}

/// 将 BaseMessage 列表转为 Anthropic messages 格式
///
/// - System 消息提取到顶层 system 字段
/// - Tool 消息合并为 user content blocks
pub(super) fn messages_to_anthropic(
    messages: &[BaseMessage],
) -> (Vec<Value>, Vec<SystemPromptBlock>) {
    let mut system_parts_with_boundary: Vec<String> = Vec::new();
    let mut system_parts_no_boundary: Vec<String> = Vec::new();
    let mut result: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            BaseMessage::System { content, .. } => {
                let text = content.text_content();
                if !text.trim().is_empty() {
                    if text.contains(cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY) {
                        system_parts_with_boundary.push(text);
                    } else {
                        system_parts_no_boundary.push(text);
                    }
                }
            }
            BaseMessage::Human { content, .. } => {
                result.push(json!({
                    "role": "user",
                    "content": content_to_anthropic(content)
                }));
            }
            BaseMessage::Ai {
                content,
                tool_calls,
                ..
            } => {
                if tool_calls.is_empty() {
                    result.push(json!({
                        "role": "assistant",
                        "content": content_to_anthropic(content)
                    }));
                } else {
                    let content_val = match content {
                        MessageContent::Blocks(_) | MessageContent::Raw(_) => {
                            content_to_anthropic(content)
                        }
                        MessageContent::Text(t) => {
                            let mut blocks: Vec<Value> = Vec::new();
                            if !t.is_empty() {
                                blocks.push(json!({ "type": "text", "text": t }));
                            }
                            for tc in tool_calls {
                                blocks.push(json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.name,
                                    "input": tc.arguments
                                }));
                            }
                            Value::Array(blocks)
                        }
                    };
                    result.push(json!({ "role": "assistant", "content": content_val }));
                }
            }
            BaseMessage::Tool {
                id: msg_id,
                tool_call_id,
                content,
                is_error,
            } => {
                let block_id = msg_id.as_uuid().to_string();
                let tool_result_block = json!({
                    "type": "tool_result",
                    "id": block_id,
                    "tool_use_id": tool_call_id,
                    "content": content_to_anthropic(content),
                    "is_error": is_error
                });

                let appended = if let Some(last) = result.last_mut() {
                    if last["role"] == "user" {
                        if let Some(arr) = last["content"].as_array_mut() {
                            arr.push(tool_result_block.clone());
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !appended {
                    result.push(json!({
                        "role": "user",
                        "content": [tool_result_block]
                    }));
                }
            }
        }
    }

    // 拼接 system
    let mut system_text = system_parts_with_boundary.join("\n\n");
    if !system_parts_no_boundary.is_empty() {
        let middleware_text = system_parts_no_boundary.join("\n\n");
        if system_text.contains(cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY) {
            system_text = system_text.replacen(
                cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
                &format!(
                    "{}\n\n{}",
                    cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
                    middleware_text
                ),
                1,
            );
        } else {
            system_text = format!("{system_text}\n\n{middleware_text}");
        }
    }
    let system_blocks = cache::split_system_blocks(&system_text);
    (result, system_blocks)
}

/// Build system blocks JSON for Anthropic API request.
pub(super) fn build_system_blocks_json(blocks: &[SystemPromptBlock]) -> Vec<Value> {
    let has_cached = blocks.iter().any(|b| b.cache_control);
    let last_idx = blocks.len().saturating_sub(1);
    blocks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut block = json!({"type": "text", "text": &b.text});
            if b.cache_control || (i == last_idx && !has_cached) {
                block["cache_control"] = json!({"type": "ephemeral"});
            }
            block
        })
        .collect()
}

#[async_trait]
impl ProviderAdapter for AnthropicAdapter {
    fn provider_name(&self) -> &str {
        "anthropic"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn context_window(&self) -> u32 {
        200_000
    }

    fn serialize_messages(&self, messages: &[BaseMessage]) -> Vec<Value> {
        let (msgs, _) = messages_to_anthropic(messages);
        msgs
    }

    fn serialize_content_block(&self, block: &ContentBlock) -> Option<Value> {
        block_to_anthropic(block)
    }

    fn serialize_tool(&self, tool: &ToolDefinition) -> Value {
        json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": tool.parameters
        })
    }

    fn build_request_body(&self, request: &LlmRequest, streaming: bool) -> Value {
        let tools_json: Vec<Value> = request
            .tools
            .iter()
            .map(|t| self.serialize_tool(t))
            .collect();

        let (mut messages, system_from_msgs) = messages_to_anthropic(&request.messages);

        // 确保所有 assistant 消息都包含 thinking block
        cache::ensure_thinking_blocks(&mut messages);

        // 合并 system blocks：消息列表中的 System（中间件注入）+ request.system
        let mut system_blocks = system_from_msgs;
        if let Some(ref base) = request.system {
            if !base.is_empty() {
                system_blocks.extend(cache::split_system_blocks(base));
            }
        }
        let max_tokens = request.max_tokens.unwrap_or(self.max_tokens);

        // 开启缓存时：对最后一条消息的最后一个 block 加 cache_control
        if self.enable_cache {
            cache::apply_cache_to_messages(&mut messages);
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "messages": messages
        });

        if streaming {
            body["stream"] = json!(true);
        }

        if self.enable_cache {
            if !system_blocks.is_empty() {
                body["system"] = Value::Array(build_system_blocks_json(&system_blocks));
            }
        } else if !system_blocks.is_empty() {
            let text = system_blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
                .replace(SYSTEM_PROMPT_DYNAMIC_BOUNDARY, "");
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                body["system"] = json!(trimmed);
            }
        }

        if !tools_json.is_empty() {
            body["tools"] = Value::Array(tools_json);
        }

        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }

        // Extended Thinking 配置
        if self.extended_thinking {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": self.thinking_budget
            });
            body["output_config"] = json!({ "effort": self.thinking_effort });
        }

        body
    }

    fn parse_response_content(
        &self,
        response_json: &Value,
    ) -> AgentResult<(Vec<ContentBlock>, Vec<ToolCallRequest>)> {
        let raw_blocks = response_json["content"]
            .as_array()
            .ok_or_else(|| AgentError::LlmError("响应缺少 content 字段".to_string()))?;
        Ok(Self::parse_content_blocks(raw_blocks))
    }

    fn extract_stop_reason(&self, response_json: &Value) -> StopReason {
        StopReason::from_display(response_json["stop_reason"].as_str().unwrap_or("end_turn"))
    }

    fn extract_usage(
        &self,
        response_json: &Value,
        request_id: Option<String>,
    ) -> Option<TokenUsage> {
        let raw_input = response_json["usage"]["input_tokens"]
            .as_u64()
            .map(|v| v as u32)
            .unwrap_or(0);
        let output = response_json["usage"]["output_tokens"]
            .as_u64()
            .map(|v| v as u32);
        let cache_creation = response_json["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .map(|v| v as u32)
            .unwrap_or(0);
        let cache_read = response_json["usage"]["cache_read_input_tokens"]
            .as_u64()
            .map(|v| v as u32)
            .unwrap_or(0);
        match (response_json["usage"]["input_tokens"].as_u64(), output) {
            (Some(_), Some(o)) => Some(TokenUsage {
                input_tokens: raw_input + cache_creation + cache_read,
                output_tokens: o,
                cache_creation_input_tokens: Some(cache_creation),
                cache_read_input_tokens: Some(cache_read),
                request_id: request_id.clone(),
            }),
            _ => None,
        }
    }

    fn extract_error_message(&self, response_json: &Value) -> String {
        response_json["error"]["message"]
            .as_str()
            .unwrap_or("未知错误")
            .to_string()
    }

    fn extract_error_type(&self, response_json: &Value) -> Option<String> {
        response_json["error"]["type"]
            .as_str()
            .map(|s| s.to_string())
    }

    fn extract_request_id_from_headers(
        &self,
        headers: &reqwest::header::HeaderMap,
    ) -> Option<String> {
        headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    }

    fn extract_request_id_from_body(&self, response_json: &Value) -> Option<String> {
        response_json["id"].as_str().map(|s| s.to_string())
    }

    fn log_success_extra(&self, response_json: &Value, _elapsed_ms: u64, _msg_count: usize) {
        let cache_read = response_json["usage"]["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        let cache_creation = response_json["usage"]["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(0);
        if cache_read > 0 || cache_creation > 0 {
            tracing::info!(cache_read, cache_creation, "Anthropic cache metrics");
        }
    }

    fn log_error_extra(&self, status: u16, response_json: &Value, body: Option<&Value>) {
        let error_type = response_json["error"]["type"].as_str().unwrap_or("unknown");
        if status == 500 {
            if let Some(b) = body {
                tracing::error!(
                    error_type,
                    request_messages = %serde_json::to_string(&b["messages"]).unwrap_or_else(|_| "serialize failed".into()),
                    "LLM API 500 错误（服务端 bug），已记录请求体"
                );
            }
        }
    }

    fn build_chat_url(&self) -> String {
        match &self.base_url {
            Some(base) => format!("{}/v1/messages", base.trim_end_matches('/')),
            None => "https://api.anthropic.com/v1/messages".to_string(),
        }
    }

    fn apply_auth_headers(
        &self,
        req: reqwest::RequestBuilder,
        session_id: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut req = req
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01");

        if self.enable_cache {
            req = req.header("anthropic-beta", "prompt-caching-2024-07-31");
        }
        if let Some(sid) = session_id {
            req = req.header("x-session-id", sid);
        }

        req
    }
}

impl AnthropicAdapter {
    /// 关联函数：从 raw JSON blocks 解析 ContentBlock + ToolCallRequest。
    ///
    /// 供 streaming 路径直接调用，避免依赖 &self 和完整的响应 JSON。
    pub(super) fn parse_content_blocks(
        raw_blocks: &[Value],
    ) -> (Vec<ContentBlock>, Vec<ToolCallRequest>) {
        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut tool_calls: Vec<ToolCallRequest> = Vec::new();

        for b in raw_blocks {
            match b["type"].as_str() {
                Some("text") => {
                    if let Some(text) = b["text"].as_str() {
                        blocks.push(ContentBlock::text(text));
                    }
                }
                Some("thinking") => {
                    let text = b["thinking"].as_str().unwrap_or("").to_string();
                    let signature = b["signature"].as_str().map(|s| s.to_string());
                    if let Some(sig) = signature {
                        blocks.push(ContentBlock::reasoning_with_signature(text, sig));
                    } else {
                        blocks.push(ContentBlock::reasoning(text));
                    }
                }
                Some("tool_use") => {
                    if let (Some(id), Some(name)) = (b["id"].as_str(), b["name"].as_str()) {
                        let input = b["input"].clone();
                        blocks.push(ContentBlock::tool_use(id, name, input.clone()));
                        tool_calls.push(ToolCallRequest::new(id, name, input));
                    }
                }
                Some("redacted_thinking") => {
                    blocks.push(ContentBlock::Unknown(b.clone()));
                }
                _ => {
                    blocks.push(ContentBlock::Unknown(b.clone()));
                }
            }
        }

        (blocks, tool_calls)
    }
}
