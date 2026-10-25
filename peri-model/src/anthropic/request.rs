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

/// Anthropic 协议允许的最小 extended-thinking 预算。
const MIN_THINKING_BUDGET: u32 = 1_024;
/// 启用 thinking 时至少为正文保留的输出预算；thinking 与正文共享输出上限。
const MIN_RESPONSE_RESERVE: u32 = 1_024;

pub(super) fn build_request(
    config: &AnthropicConfig,
    request: &ModelRequest,
) -> ModelResult<BuiltAnthropicRequest> {
    let (mut messages, mut system_prompt) = messages_to_anthropic(&request.messages);
    cache::ensure_thinking_blocks(&mut messages);
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
        // 协议约束：budget_tokens 必须小于 max_tokens，且不低于最小预算。
        // thinking 与正文共享输出上限，因此还要为正文预留 MIN_RESPONSE_RESERVE；
        // 请求级 max_tokens 覆盖（验证器、分类器等派生请求）可能放不下
        // 最小思考预算 + 正文预留，此时不发送 thinking。
        let available_thinking = max_tokens.saturating_sub(MIN_RESPONSE_RESERVE);
        let thinking_budget = config.thinking_budget.min(available_thinking);
        if thinking_budget >= MIN_THINKING_BUDGET {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": thinking_budget,
            });
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

fn messages_to_anthropic(messages: &[ModelMessage]) -> (Vec<Value>, cache::SplitSystemPrompt) {
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
                "content": content_to_anthropic(content),
            })),
            ModelMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut parts = content_to_anthropic_parts(content);
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
                let block = tool_result_to_anthropic(tool_result);
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

fn tool_result_to_anthropic(result: &ToolResult) -> Value {
    json!({
        "type": "tool_result",
        "id": result.id.clone().unwrap_or_else(|| result.tool_call_id.clone()),
        "tool_use_id": result.tool_call_id,
        "content": content_to_anthropic(&result.content),
        "is_error": result.is_error,
    })
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(ContentBlock::text_content)
        .collect()
}

fn content_to_anthropic(content: &[ContentBlock]) -> Value {
    Value::Array(content_to_anthropic_parts(content))
}

fn content_to_anthropic_parts(content: &[ContentBlock]) -> Vec<Value> {
    let foreign_replay = content
        .iter()
        .any(|block| matches!(block, ContentBlock::ProviderState { .. }));
    content
        .iter()
        .filter_map(|block| match block {
            // A foreign reasoning summary is readable history, not an
            // Anthropic thinking block with an Anthropic replay signature.
            ContentBlock::Reasoning { text, .. } if foreign_replay => {
                Some(json!({ "type": "text", "text": text }))
            }
            _ => block_to_anthropic(block),
        })
        .collect()
}

fn block_to_anthropic(block: &ContentBlock) -> Option<Value> {
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
        ContentBlock::ToolResult { result } => Some(tool_result_to_anthropic(result)),
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
