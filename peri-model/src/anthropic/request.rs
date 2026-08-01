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

pub(super) fn build_request(
    config: &AnthropicConfig,
    request: &ModelRequest,
) -> ModelResult<BuiltAnthropicRequest> {
    let (mut messages, mut system_blocks) = messages_to_anthropic(&request.messages);
    cache::ensure_thinking_blocks(&mut messages);
    if config.enable_cache {
        cache::apply_cache_to_messages(&mut messages);
    }

    let mut body = json!({
        "model": config.model,
        "max_tokens": request.max_tokens.unwrap_or(config.max_tokens),
        "messages": messages,
        "stream": true,
    });
    if config.enable_cache && !system_blocks.is_empty() {
        body["system"] = Value::Array(cache::system_blocks_to_json(&system_blocks));
    } else if !config.enable_cache && !system_blocks.is_empty() {
        let system = system_blocks
            .drain(..)
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n\n")
            .replace(cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY, "");
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
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": config.thinking_budget,
        });
        body["output_config"] = json!({ "effort": config.thinking_effort });
    }

    Ok(BuiltAnthropicRequest {
        endpoint: messages_endpoint(&config.endpoint)?,
        body,
        session_id: request.session_id.clone(),
        model_id: config.model.clone(),
    })
}

pub(super) fn messages_endpoint(endpoint: &Url) -> ModelResult<Url> {
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
    {
        return Err(ModelError::protocol(
            crate::ProtocolErrorKind::InvalidEndpoint,
        ));
    }
    let mut endpoint = endpoint.clone();
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    let mut path_segments = endpoint
        .path_segments_mut()
        .map_err(|_| ModelError::protocol(crate::ProtocolErrorKind::InvalidEndpoint))?;
    path_segments.pop_if_empty();
    path_segments.push("v1");
    path_segments.push("messages");
    drop(path_segments);
    Ok(endpoint)
}

fn messages_to_anthropic(messages: &[ModelMessage]) -> (Vec<Value>, Vec<cache::SystemPromptBlock>) {
    let mut system_with_boundary = Vec::new();
    let mut system_without_boundary = Vec::new();
    let mut result = Vec::new();

    for message in messages {
        match message {
            ModelMessage::System { content } => {
                let text = content_text(content);
                if !text.trim().is_empty() {
                    if text.contains(cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY) {
                        system_with_boundary.push(text);
                    } else {
                        system_without_boundary.push(text);
                    }
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

    let mut system = system_with_boundary.join("\n\n");
    if !system_without_boundary.is_empty() {
        let middleware = system_without_boundary.join("\n\n");
        if system.contains(cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY) {
            system = system.replacen(
                cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
                &format!("{}\n\n{middleware}", cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY),
                1,
            );
        } else {
            system = format!("{system}\n\n{middleware}");
        }
    }
    (result, cache::split_system_blocks(&system))
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
    content.iter().filter_map(block_to_anthropic).collect()
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
    }
}

#[cfg(test)]
pub(super) fn body_for_test(config: &AnthropicConfig, request: &ModelRequest) -> Value {
    build_request(config, request)
        .expect("test config is valid")
        .body
}
