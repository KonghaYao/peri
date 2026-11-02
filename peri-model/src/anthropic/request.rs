use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use url::Url;

use crate::{
    ContentBlock, DocumentSource, ImageSource, ModelError, ModelMessage, ModelRequest, ModelResult,
    PreparedModelRequest, ProviderProtocol, ToolCall, ToolDefinition, ToolResult,
};

use super::{cache, AnthropicConfig};

#[derive(Clone)]
pub(super) struct BuiltAnthropicRequest {
    pub(super) endpoint: Url,
    pub(super) body: Value,
    pub(super) session_id: Option<String>,
    model_id: String,
}

impl BuiltAnthropicRequest {
    pub(super) fn observe(
        &self,
        runtime: &crate::ModelRuntimeConfig,
    ) -> ModelResult<PreparedModelRequest> {
        PreparedModelRequest::observe_with_runtime(
            ProviderProtocol::Anthropic,
            self.model_id.clone(),
            self.endpoint.clone(),
            self.body.clone(),
            BTreeMap::new(),
            runtime,
        )
    }
}

/// Anthropic 协议允许的最小手动 thinking 预算。
const MIN_THINKING_BUDGET: u32 = 1_024;
/// 手动 thinking 至少为正文保留的输出预算；thinking 与正文共享输出上限。
const MIN_RESPONSE_RESERVE: u32 = 1_024;

/// 历史 reasoning 回放策略，由端点与模型共同决定。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ThinkingReplay {
    /// 官方 Anthropic：只回放带非空 signature 的 thinking，不伪造占位块。
    Signed,
    /// 兼容端点：不校验 signature，history 中 assistant 消息必须携带 thinking 块。
    Verbatim,
}

impl ThinkingReplay {
    /// 端点身份优先于模型名：DeepSeek 接受 claude* 别名并映射到自家模型，
    /// 但回放规则仍由端点决定。
    fn for_endpoint(endpoint: &Url, model: &str) -> Self {
        let host = endpoint.host_str().unwrap_or_default().to_ascii_lowercase();
        if host == "deepseek.com" || host.ends_with(".deepseek.com") {
            return Self::Verbatim;
        }
        if host == "api.anthropic.com" {
            return Self::Signed;
        }
        let model = model.to_ascii_lowercase();
        // 覆盖 OpenRouter 的 `anthropic/claude-*` 形式。
        if model.starts_with("claude") || model.contains("/claude") {
            Self::Signed
        } else {
            Self::Verbatim
        }
    }
}

pub(super) fn build_request(
    config: &AnthropicConfig,
    request: &ModelRequest,
) -> ModelResult<BuiltAnthropicRequest> {
    let replay = ThinkingReplay::for_endpoint(&config.endpoint, &config.model);
    let (mut messages, mut system_prompt) = messages_to_anthropic(&request.messages, replay);
    if replay == ThinkingReplay::Verbatim {
        // DeepSeek 等兼容端点要求 thinking 模式下的历史 assistant 消息携带 thinking 块，
        // 且不校验输入 signature；缺块时用空 thinking 占位补齐。占位须在 cache 处理前插入。
        cache::ensure_thinking_blocks(&mut messages);
    }
    if config.enable_cache {
        cache::apply_cache_to_messages(&mut messages);
    }

    let max_tokens = request.max_tokens.unwrap_or(config.max_tokens);
    let mut body = json!({
        "model": config.model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": true,
    });
    if config.enable_cache && !system_prompt.blocks.is_empty() {
        body["system"] = Value::Array(cache::system_blocks_to_json(
            &system_prompt.blocks,
            system_prompt.allow_fallback_cache,
        ));
    } else if !config.enable_cache && !system_prompt.blocks.is_empty() {
        let system = system_prompt
            .blocks
            .drain(..)
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n\n");
        if !system.trim().is_empty() {
            body["system"] = json!(system.trim());
        }
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(tool_to_anthropic).collect());
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if config.extended_thinking {
        let thinking = if uses_adaptive_thinking(&config.model) {
            // 仅对已确认的精确模型使用 adaptive；不发送手动预算，也不应用其门限。
            Some(json!({ "type": "adaptive" }))
        } else {
            // 手动预算必须不低于最小值，并为正文预留 MIN_RESPONSE_RESERVE。
            // 请求级 max_tokens 覆盖可能放不下两者，此时不发送 thinking。
            let available_thinking = max_tokens.saturating_sub(MIN_RESPONSE_RESERVE);
            let thinking_budget = config.thinking_budget.min(available_thinking);
            // 手动 thinking 要求最后一个 assistant turn 以带签名的 thinking 块开始。
            // 旧历史没有可验证的签名块，伪造空签名会被 API 拒绝，此时按官方建议不启用
            // thinking；兼容端点不校验签名且强制回放，不受该门控约束。
            (thinking_budget >= MIN_THINKING_BUDGET
                && (replay == ThinkingReplay::Verbatim
                    || final_assistant_turn_starts_with_thinking(&messages)))
            .then(|| {
                json!({
                    "type": "enabled",
                    "budget_tokens": thinking_budget,
                })
            })
        };
        if let Some(thinking) = thinking {
            body["thinking"] = thinking;
            body["output_config"] = json!({ "effort": config.thinking_effort });
        }
    }

    Ok(BuiltAnthropicRequest {
        endpoint: messages_endpoint(&config.endpoint)?,
        body,
        session_id: request.session_id.clone(),
        model_id: config.model.clone(),
    })
}

pub(super) fn messages_endpoint(endpoint: &Url) -> ModelResult<Url> {
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err(ModelError::protocol(
            crate::ProtocolErrorKind::InvalidEndpoint,
        ));
    }
    let mut endpoint = endpoint.clone();
    let mut path_segments = endpoint.path_segments_mut().map_err(|_| {
        ModelError::protocol_with_summary(
            crate::ProtocolErrorKind::InvalidEndpoint,
            "URL has no hierarchical path",
        )
    })?;
    path_segments.pop_if_empty();
    path_segments.push("v1");
    path_segments.push("messages");
    drop(path_segments);
    Ok(endpoint)
}

fn messages_to_anthropic(
    messages: &[ModelMessage],
    replay: ThinkingReplay,
) -> (Vec<Value>, cache::SplitSystemPrompt) {
    let mut system = Vec::new();
    let mut result = Vec::new();

    for message in messages {
        match message {
            ModelMessage::System { content } => {
                let text = content_text(content);
                if !text.trim().is_empty() {
                    system.push(text);
                }
            }
            ModelMessage::User { content } => result.push(json!({
                "role": "user",
                "content": content_to_anthropic(content, replay),
            })),
            ModelMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut parts = content_to_anthropic_parts(content, replay);
                let content_tool_use_ids = content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolUse { tool_call } => Some(tool_call.id()),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>();
                parts.extend(
                    tool_calls
                        .iter()
                        .filter(|tool_call| !content_tool_use_ids.contains(tool_call.id()))
                        .map(tool_call_to_anthropic),
                );
                result.push(json!({ "role": "assistant", "content": parts }));
            }
            ModelMessage::ToolResult {
                result: tool_result,
            } => {
                let block = tool_result_to_anthropic(tool_result, replay);
                let appended = result.last_mut().is_some_and(|message: &mut Value| {
                    if message["role"] != "user" {
                        return false;
                    }
                    message["content"]
                        .as_array_mut()
                        .map(|content| content.push(block.clone()))
                        .is_some()
                });
                if !appended {
                    result.push(json!({ "role": "user", "content": [block] }));
                }
            }
        }
    }

    (result, cache::split_system_blocks(&system.join("\n\n")))
}

fn tool_to_anthropic(tool: &ToolDefinition) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema.as_map(),
    })
}

fn tool_call_to_anthropic(tool_call: &ToolCall) -> Value {
    json!({
        "type": "tool_use",
        "id": tool_call.id(),
        "name": tool_call.name(),
        "input": tool_call.arguments().as_map(),
    })
}

fn tool_result_to_anthropic(result: &ToolResult, replay: ThinkingReplay) -> Value {
    json!({
        "type": "tool_result",
        "id": result.id.clone().unwrap_or_else(|| result.tool_call_id.clone()),
        "tool_use_id": result.tool_call_id,
        "content": content_to_anthropic(&result.content, replay),
        "is_error": result.is_error,
    })
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(ContentBlock::text_content)
        .collect()
}

fn content_to_anthropic(content: &[ContentBlock], replay: ThinkingReplay) -> Value {
    Value::Array(content_to_anthropic_parts(content, replay))
}

fn content_to_anthropic_parts(content: &[ContentBlock], replay: ThinkingReplay) -> Vec<Value> {
    let foreign_replay = content
        .iter()
        .any(|block| matches!(block, ContentBlock::ProviderState { .. }));
    content
        .iter()
        .filter_map(|block| match block {
            // 官方 Anthropic 只接受由它签名、可原样回放的 thinking 块；其他 provider
            // 的 reasoning 摘要跨协议回放时一律降级为文本，避免泄漏原生状态。
            ContentBlock::Reasoning { text, signature }
                if foreign_replay
                    || (replay == ThinkingReplay::Signed && !has_replay_signature(signature)) =>
            {
                (!text.is_empty()).then(|| json!({ "type": "text", "text": text }))
            }
            _ => block_to_anthropic(block, replay),
        })
        .collect()
}

/// 已确认只支持 adaptive thinking、拒绝手动 `enabled` 的精确模型。
fn uses_adaptive_thinking(model: &str) -> bool {
    model == "claude-sonnet-5-5"
}

fn has_replay_signature(signature: &Option<String>) -> bool {
    signature
        .as_deref()
        .is_some_and(|signature| !signature.trim().is_empty())
}

/// 手动 thinking 要求请求中最后一个 assistant turn 以 thinking 块开始。
/// tool-use 循环属于同一个 assistant turn：工具结果之后的 assistant 消息只是
/// 该 turn 的延续，判定必须回溯到 turn 起点，而不是只看最后一条 assistant 消息。
/// 没有 assistant turn 时不存在该约束。
fn final_assistant_turn_starts_with_thinking(messages: &[Value]) -> bool {
    let Some(mut index) = messages
        .iter()
        .rposition(|message| message["role"] == "assistant")
    else {
        return true;
    };
    while index > 0 {
        let preceding = &messages[index - 1];
        if preceding["role"] != "user" || !contains_tool_result(preceding) {
            break;
        }
        let Some(turn_start) = messages[..index - 1]
            .iter()
            .rposition(|message| message["role"] == "assistant")
        else {
            break;
        };
        index = turn_start;
    }
    messages[index]["content"]
        .as_array()
        .and_then(|blocks| blocks.first())
        .is_some_and(is_replay_thinking_block)
}

fn contains_tool_result(message: &Value) -> bool {
    message["content"]
        .as_array()
        .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "tool_result"))
}

fn is_replay_thinking_block(block: &Value) -> bool {
    match block["type"].as_str() {
        Some("redacted_thinking") => true,
        Some("thinking") => block["signature"]
            .as_str()
            .is_some_and(|signature| !signature.trim().is_empty()),
        _ => false,
    }
}

fn block_to_anthropic(block: &ContentBlock, replay: ThinkingReplay) -> Option<Value> {
    match block {
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::Image { source } => match source {
            ImageSource::Base64 { media_type, data } => Some(json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type.as_str(), "data": data },
            })),
            ImageSource::Url { url } => Some(json!({
                "type": "image",
                "source": { "type": "url", "url": url },
            })),
        },
        ContentBlock::Document { source, title } => {
            let source = match source {
                DocumentSource::Base64 { media_type, data } => json!({
                    "type": "base64", "media_type": media_type.as_str(), "data": data,
                }),
                DocumentSource::Url { url } => json!({ "type": "url", "url": url }),
                DocumentSource::Text { text } => json!({ "type": "text", "text": text }),
            };
            Some(json!({ "type": "document", "source": source, "title": title }))
        }
        ContentBlock::Reasoning { text, signature } => {
            let mut value = json!({ "type": "thinking", "thinking": text });
            if let Some(signature) = signature {
                value["signature"] = json!(signature);
            }
            Some(value)
        }
        ContentBlock::ToolUse { tool_call } => Some(tool_call_to_anthropic(tool_call)),
        ContentBlock::ToolResult { result } => Some(tool_result_to_anthropic(result, replay)),
        ContentBlock::RedactedReasoning { data } => Some(json!({
            "type": "redacted_thinking",
            "data": data,
        })),
        // A different adapter owns this replay state. Typed text and calls are
        // converted normally; foreign encrypted items never enter this wire.
        ContentBlock::ProviderState { .. } => None,
    }
}

#[cfg(test)]
pub(super) fn body_for_test(config: &AnthropicConfig, request: &ModelRequest) -> Value {
    build_request(config, request)
        .expect("test config is valid")
        .body
}
