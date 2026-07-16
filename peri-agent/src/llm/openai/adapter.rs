use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{
    error::AgentResult,
    llm::provider_adapter::ProviderAdapter,
    llm::types::{LlmRequest, StopReason, TokenUsage},
    messages::{BaseMessage, ContentBlock, ImageSource, MessageContent, ToolCallRequest},
    tools::ToolDefinition,
};

/// OpenAiAdapter — 所有 OpenAI Provider 特定逻辑
pub(super) struct OpenAiAdapter {
    pub(super) api_key: String,
    pub(super) base_url: String,
    pub(super) model: String,
    pub(super) reasoning_effort: Option<String>,
    pub(super) thinking_enabled: bool,
    pub(super) supports_thinking_content: bool,
    pub(super) max_tokens: u32,
}

// ─── ContentBlock → OpenAI content part ────────────────────────────────────

fn block_to_openai_part(block: &ContentBlock, supports_thinking_content: bool) -> Option<Value> {
    match block {
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::Image { source } => {
            let image_url = match source {
                ImageSource::Url { url } => json!({ "url": url }),
                ImageSource::Base64 { media_type, data } => {
                    json!({ "url": format!("data:{media_type};base64,{data}") })
                }
            };
            Some(json!({ "type": "image_url", "image_url": image_url }))
        }
        ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => None,
        ContentBlock::Reasoning { text, signature } if supports_thinking_content => {
            let mut obj = json!({ "type": "thinking", "thinking": text });
            if let Some(sig) = signature {
                obj["signature"] = json!(sig);
            }
            Some(obj)
        }
        ContentBlock::Reasoning { .. } => None,
        ContentBlock::Document { source, title } => {
            let src = serde_json::to_value(source).unwrap_or_default();
            Some(json!({ "type": "document", "source": src, "title": title }))
        }
        ContentBlock::Unknown(v) => Some(v.clone()),
    }
}

pub(super) fn content_to_openai(
    content: &MessageContent,
    supports_thinking_content: bool,
) -> Value {
    match content {
        MessageContent::Text(s) => json!(s),
        MessageContent::Blocks(blocks) => {
            let parts: Vec<Value> = blocks
                .iter()
                .filter_map(|b| block_to_openai_part(b, supports_thinking_content))
                .collect();
            if parts.is_empty() {
                json!("")
            } else {
                Value::Array(parts)
            }
        }
        MessageContent::Raw(values) => {
            if supports_thinking_content {
                Value::Array(values.clone())
            } else {
                let filtered: Vec<Value> = values
                    .iter()
                    .filter(|v| {
                        let t = v["type"].as_str().unwrap_or("");
                        t != "thinking" && t != "reasoning"
                    })
                    .cloned()
                    .collect();
                if filtered.is_empty() {
                    json!("")
                } else {
                    Value::Array(filtered)
                }
            }
        }
    }
}

fn extract_reasoning_text(content: &MessageContent) -> Option<String> {
    match content {
        MessageContent::Blocks(blocks) => {
            let parts: Vec<&str> = blocks.iter().filter_map(|b| b.as_reasoning()).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(""))
            }
        }
        _ => None,
    }
}

/// 将 BaseMessage 列表转为 OpenAI messages 格式
pub(super) fn messages_to_json(adapter: &OpenAiAdapter, messages: &[BaseMessage]) -> Vec<Value> {
    let mut system_parts: Vec<String> = Vec::new();
    let mut result: Vec<Value> = Vec::new();

    for m in messages {
        match m {
            BaseMessage::System { content, .. } => {
                let t = content.text_content();
                if !t.trim().is_empty() {
                    system_parts.push(t);
                }
            }
            BaseMessage::Human { content, .. } => {
                result.push(json!({
                    "role": "user",
                    "content": content_to_openai(content, adapter.supports_thinking_content)
                }));
            }
            BaseMessage::Ai {
                content,
                tool_calls,
                ..
            } => {
                let reasoning_text = extract_reasoning_text(content);
                let serialized_content =
                    content_to_openai(content, adapter.supports_thinking_content);

                if tool_calls.is_empty() {
                    let mut msg = json!({ "role": "assistant", "content": serialized_content });
                    let reasoning_val = json!(reasoning_text.as_deref().unwrap_or(""));
                    msg["reasoning_content"] = reasoning_val;
                    result.push(msg);
                } else {
                    let tcs: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments.to_string()
                                }
                            })
                        })
                        .collect();
                    let mut msg = json!({
                        "role": "assistant",
                        "content": serialized_content,
                        "tool_calls": tcs
                    });
                    let reasoning_val = json!(reasoning_text.as_deref().unwrap_or(""));
                    msg["reasoning_content"] = reasoning_val;
                    result.push(msg);
                }
            }
            BaseMessage::Tool {
                tool_call_id,
                content,
                ..
            } => {
                result.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content_to_openai(content, adapter.supports_thinking_content)
                }));
            }
        }
    }

    if !system_parts.is_empty() {
        let system_text = system_parts
            .join("\n\n")
            .replace("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__", "");
        result.insert(0, json!({ "role": "system", "content": system_text }));
    }

    result
}

/// 校验消息序列不变量：每段连续 tool 消息块之前必须有 assistant with tool_calls
pub(super) fn validate_message_invariants(messages: &[Value]) {
    let mut i = 0;
    while i < messages.len() {
        if messages[i]["role"] == "tool" {
            let block_start = i;
            let prev_non_tool = if block_start > 0 {
                let mut j = block_start;
                while j > 0 && messages[j - 1]["role"] == "tool" {
                    j -= 1;
                }
                if j > 0 {
                    Some(&messages[j - 1])
                } else {
                    None
                }
            } else {
                None
            };
            let valid = prev_non_tool
                .is_some_and(|p| p["role"] == "assistant" && p["tool_calls"].is_array());
            if !valid {
                tracing::error!(
                    block_start,
                    total = messages.len(),
                    prev_non_tool_role = ?prev_non_tool.map(|m| m["role"].as_str()),
                    "消息序列不变量违反：连续 tool 块前缺少 assistant with tool_calls"
                );
            }
            while i < messages.len() && messages[i]["role"] == "tool" {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
}

#[async_trait]
impl ProviderAdapter for OpenAiAdapter {
    fn provider_name(&self) -> &str {
        "openai"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn context_window(&self) -> u32 {
        200_000
    }

    fn serialize_messages(&self, messages: &[BaseMessage]) -> Vec<Value> {
        messages_to_json(self, messages)
    }

    fn serialize_content_block(&self, block: &ContentBlock) -> Option<Value> {
        block_to_openai_part(block, self.supports_thinking_content)
    }

    fn serialize_tool(&self, tool: &ToolDefinition) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters
            }
        })
    }

    fn build_request_body(&self, request: &LlmRequest, streaming: bool) -> Value {
        let tools_json: Vec<Value> = request
            .tools
            .iter()
            .map(|t| self.serialize_tool(t))
            .collect();

        let mut messages = messages_to_json(self, &request.messages);

        validate_message_invariants(&messages);

        if let Some(base_system) = &request.system {
            if let Some(first) = messages.first_mut() {
                if first["role"] == "system" {
                    let existing = first["content"].as_str().unwrap_or("");
                    first["content"] = json!(format!("{}\n\n{}", existing, base_system));
                } else {
                    messages.insert(0, json!({ "role": "system", "content": base_system }));
                }
            } else {
                messages.insert(0, json!({ "role": "system", "content": base_system }));
            }
        }

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": streaming
        });

        // Qwen 兼容 API 需要通过 stream_options.include_usage 在流式末尾获取 Token 消耗
        if streaming && self.model.to_lowercase().contains("qwen") {
            body["stream_options"] = json!({"include_usage": true});
        }

        if !tools_json.is_empty() {
            body["tools"] = Value::Array(tools_json);
            body["tool_choice"] = json!("auto");
        }

        let max_tokens = request.max_tokens.unwrap_or(self.max_tokens);
        body["max_tokens"] = json!(max_tokens);

        if let Some(ref effort) = self.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        } else if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }

        // DeepSeek thinking 模式
        if self.thinking_enabled {
            body["thinking"] = json!({ "type": "enabled" });
            if self.model.to_lowercase().contains("kimi") {
                body.as_object_mut()
                    .and_then(|b| b.remove("reasoning_effort"));
            }
        }

        // LiteLLM session tracking
        if let Some(ref sid) = request.session_id {
            body["metadata"] = json!({ "session_id": sid });
        }

        body
    }

    fn parse_response_content(
        &self,
        response_json: &Value,
    ) -> AgentResult<(Vec<ContentBlock>, Vec<ToolCallRequest>)> {
        let assistant_msg = &response_json["choices"][0]["message"];
        Ok(parse_content_from_assistant(assistant_msg))
    }

    fn extract_stop_reason(&self, response_json: &Value) -> StopReason {
        let finish_reason = response_json["choices"][0]["finish_reason"]
            .as_str()
            .unwrap_or("stop");
        StopReason::from_openai(finish_reason)
    }

    fn extract_usage(
        &self,
        response_json: &Value,
        request_id: Option<String>,
    ) -> Option<TokenUsage> {
        extract_openai_usage_inner(&response_json["usage"], request_id)
    }

    fn extract_error_message(&self, response_json: &Value) -> String {
        response_json["error"]["message"]
            .as_str()
            .unwrap_or("未知错误")
            .to_string()
    }

    fn extract_error_type(&self, response_json: &Value) -> Option<String> {
        // OpenAI 的 error_code（如 "context_length_exceeded"、"rate_limit_exceeded"）
        // 落在 response["error"]["code"]——与 Anthropic 的 "type" 字段语义一致。
        response_json["error"]["code"]
            .as_str()
            .map(|s| s.to_string())
    }

    fn extract_request_id_from_headers(
        &self,
        _headers: &reqwest::header::HeaderMap,
    ) -> Option<String> {
        // OpenAI 在 body 中返回 id，不在 headers
        None
    }

    fn extract_request_id_from_body(&self, response_json: &Value) -> Option<String> {
        response_json["id"].as_str().map(|s| s.to_string())
    }

    fn build_chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    fn apply_auth_headers(
        &self,
        req: reqwest::RequestBuilder,
        _session_id: Option<&str>,
    ) -> reqwest::RequestBuilder {
        req.bearer_auth(&self.api_key)
    }
}

/// 从 assistant message JSON 无条件解析 ContentBlock + ToolCallRequest。
///
/// 不依赖 stop_reason 做分支——调用方（GenericInvoker）在使用 blocks/tool_calls
/// 时会结合 stop_reason 做最终决策。
fn parse_content_from_assistant(
    assistant_msg: &Value,
) -> (Vec<ContentBlock>, Vec<ToolCallRequest>) {
    let content_val = &assistant_msg["content"];
    let is_array = content_val.is_array();

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();

    // 1) reasoning_content 顶层字段（deepseek-r1、某些 OpenAI o 系列）
    let mut has_top_level_reasoning = false;
    let reasoning_text = assistant_msg["reasoning_content"]
        .as_str()
        .or_else(|| assistant_msg["reasoning"].as_str());
    if let Some(reasoning) = reasoning_text {
        if !reasoning.is_empty() {
            blocks.push(ContentBlock::reasoning(reasoning));
            has_top_level_reasoning = true;
        }
    }

    if is_array {
        if let Some(arr) = content_val.as_array() {
            for item in arr {
                let item_type = item["type"].as_str().unwrap_or("");
                match item_type {
                    "thinking" if !has_top_level_reasoning => {
                        if let Some(thinking_text) = item["thinking"].as_str() {
                            if !thinking_text.is_empty() {
                                blocks.push(ContentBlock::reasoning(thinking_text));
                            }
                        }
                    }
                    "text" => {
                        if let Some(t) = item["text"].as_str() {
                            if !t.is_empty() {
                                text_parts.push(t.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    } else {
        let content_str = content_val.as_str().unwrap_or("");
        if !content_str.is_empty() {
            text_parts.push(content_str.to_string());
        }
    }

    // 合并文本
    let content_str = text_parts.join("");
    if !content_str.is_empty() {
        blocks.push(ContentBlock::text(&content_str));
    }

    // 无条件解析 tool_calls（不依赖 stop_reason）
    let tool_calls: Vec<ToolCallRequest> = assistant_msg["tool_calls"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|tc| {
            let id = tc["id"].as_str()?;
            let name = tc["function"]["name"].as_str()?;
            let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
            let arguments = match serde_json::from_str::<Value>(args_str) {
                Ok(v) => v,
                Err(_) => {
                    tracing::warn!(
                        tool = name,
                        raw_args = %args_str,
                        "OpenAI tool_call arguments JSON 解析失败，使用空对象"
                    );
                    serde_json::json!({"_raw_arguments": args_str})
                }
            };
            blocks.push(ContentBlock::tool_use(id, name, arguments.clone()));
            Some(ToolCallRequest::new(id, name, arguments))
        })
        .collect();

    (blocks, tool_calls)
}

/// parse_assistant_message 的向后兼容包装：先解析 blocks + tool_calls，
/// 再按 stop_reason 构建 BaseMessage。
///
/// 用于测试文件的 thin wrapper 和 invoke.rs 的向后兼容路径。
#[allow(dead_code)]
pub(super) fn parse_assistant_message(
    assistant_msg: &Value,
    stop_reason: &StopReason,
) -> crate::messages::BaseMessage {
    let (blocks, tool_calls) = parse_content_from_assistant(assistant_msg);

    // 重建 content_str 用于简单 Text 变体判定
    let content_str = blocks
        .iter()
        .filter_map(|b| b.as_text())
        .collect::<Vec<_>>()
        .join("");

    if *stop_reason == StopReason::ToolUse {
        let content = if blocks.len() == 1 && blocks[0].as_text().is_some() {
            MessageContent::text(content_str)
        } else if blocks.is_empty() {
            MessageContent::default()
        } else {
            MessageContent::Blocks(blocks)
        };

        BaseMessage::ai_with_tool_calls(content, tool_calls)
    } else if blocks.len() == 1 && blocks[0].as_text().is_some() {
        BaseMessage::ai(content_str)
    } else if blocks.is_empty() {
        BaseMessage::ai("")
    } else {
        BaseMessage::ai(MessageContent::Blocks(blocks))
    }
}

/// 从 OpenAI API 响应中提取 TokenUsage
fn extract_openai_usage_inner(usage_val: &Value, request_id: Option<String>) -> Option<TokenUsage> {
    let input = usage_val["prompt_tokens"].as_u64().map(|v| v as u32);
    let output = usage_val["completion_tokens"].as_u64().map(|v| v as u32);
    let cache_read = usage_val["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .map(|v| v as u32);
    match (input, output) {
        (Some(i), Some(o)) => Some(TokenUsage {
            input_tokens: i,
            output_tokens: o,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: cache_read,
            request_id,
        }),
        _ => None,
    }
}
