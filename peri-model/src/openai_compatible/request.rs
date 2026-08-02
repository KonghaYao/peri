use std::collections::BTreeMap;

use serde_json::{json, Value};
use url::Url;

use crate::{
    ContentBlock, DocumentSource, ImageSource, ModelError, ModelMessage, ModelRequest, ModelResult,
    PreparedModelRequest, ProviderProtocol, ToolDefinition,
};

use super::OpenAiConfig;

#[derive(Clone)]
pub(super) struct BuiltOpenAiRequest {
    pub(super) endpoint: Url,
    pub(super) body: Value,
    model_id: String,
}

impl BuiltOpenAiRequest {
    pub(super) fn observe(
        &self,
        runtime: &crate::ModelRuntimeConfig,
    ) -> ModelResult<PreparedModelRequest> {
        PreparedModelRequest::observe_with_runtime(
            ProviderProtocol::OpenAiCompatible,
            self.model_id.clone(),
            self.endpoint.clone(),
            self.body.clone(),
            BTreeMap::new(),
            runtime,
        )
    }
}

pub(super) fn build_request(
    config: &OpenAiConfig,
    request: &ModelRequest,
) -> ModelResult<BuiltOpenAiRequest> {
    let mut messages = messages_to_json(config, &request.messages);
    let system = extract_system_message(&request.messages);
    if let Some(system) = system.filter(|content| !content.trim().is_empty()) {
        messages.retain(|message| message["role"] != "system");
        messages.insert(0, json!({ "role": "system", "content": system }));
    }

    let mut body = json!({
        "model": config.model,
        "messages": messages,
        "stream": true,
        "max_tokens": request.max_tokens.unwrap_or(config.max_tokens),
    });

    if config.model.to_ascii_lowercase().contains("qwen") {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(tool_to_openai).collect());
        body["tool_choice"] = json!("auto");
    }
    if let Some(reasoning_effort) = &config.reasoning_effort {
        body["reasoning_effort"] = json!(reasoning_effort);
    } else if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if config.thinking_enabled {
        body["thinking"] = json!({ "type": "enabled" });
        if config.model.to_ascii_lowercase().contains("kimi") {
            body.as_object_mut()
                .expect("OpenAI request body is an object")
                .remove("reasoning_effort");
        }
    }
    if let Some(session_id) = &request.session_id {
        body["metadata"] = json!({ "session_id": session_id });
    }

    Ok(BuiltOpenAiRequest {
        endpoint: chat_completions_endpoint(&config.endpoint)?,
        body,
        model_id: config.model.clone(),
    })
}

pub(super) fn chat_completions_endpoint(endpoint: &Url) -> ModelResult<Url> {
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
    path_segments.push("chat");
    path_segments.push("completions");
    drop(path_segments);
    Ok(endpoint)
}

fn extract_system_message(messages: &[ModelMessage]) -> Option<String> {
    let parts = messages
        .iter()
        .filter_map(|message| match message {
            ModelMessage::System { content } => Some(content_text(content)),
            _ => None,
        })
        .filter(|content| !content.trim().is_empty())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| {
        parts
            .join("\n\n")
            .replace("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__", "")
    })
}

fn messages_to_json(config: &OpenAiConfig, messages: &[ModelMessage]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|message| match message {
            ModelMessage::System { .. } => None,
            ModelMessage::User { content } => Some(json!({
                "role": "user",
                "content": content_to_openai(content, config.supports_thinking_content),
            })),
            ModelMessage::Assistant {
                content,
                tool_calls,
            } => {
                let reasoning = reasoning_text(content).unwrap_or_default();
                let mut message = json!({
                    "role": "assistant",
                    "content": content_to_openai(content, config.supports_thinking_content),
                    "reasoning_content": reasoning,
                });
                if !tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(
                        tool_calls
                            .iter()
                            .map(|tool_call| {
                                json!({
                                    "id": tool_call.id(),
                                    "type": "function",
                                    "function": {
                                        "name": tool_call.name(),
                                        "arguments": serde_json::to_string(tool_call.arguments().as_map())
                                            .expect("JsonObject always serializes"),
                                    },
                                })
                            })
                            .collect(),
                    );
                }
                Some(message)
            }
            ModelMessage::ToolResult { result } => Some(json!({
                "role": "tool",
                "tool_call_id": result.tool_call_id,
                "content": content_to_openai(&result.content, config.supports_thinking_content),
            })),
        })
        .collect()
}

fn tool_to_openai(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema.as_map(),
        },
    })
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(ContentBlock::text_content)
        .collect()
}

fn reasoning_text(content: &[ContentBlock]) -> Option<String> {
    let reasoning = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    (!reasoning.is_empty()).then_some(reasoning)
}

fn content_to_openai(content: &[ContentBlock], supports_thinking_content: bool) -> Value {
    if let [ContentBlock::Text { text }] = content {
        return json!(text);
    }
    let parts = content
        .iter()
        .filter_map(|block| block_to_openai_part(block, supports_thinking_content))
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [] => json!(""),
        [Value::String(text)] => json!(text),
        _ => Value::Array(parts),
    }
}

fn block_to_openai_part(block: &ContentBlock, supports_thinking_content: bool) -> Option<Value> {
    match block {
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::Image { source } => {
            let image_url = match source {
                ImageSource::Url { url } => json!({ "url": url }),
                ImageSource::Base64 { media_type, data } => {
                    json!({ "url": format!("data:{};base64,{data}", media_type.as_str()) })
                }
            };
            Some(json!({ "type": "image_url", "image_url": image_url }))
        }
        ContentBlock::Document { source, title } => {
            let source = match source {
                DocumentSource::Url { url } => json!({ "type": "url", "url": url }),
                DocumentSource::Text { text } => json!({ "type": "text", "text": text }),
                DocumentSource::Base64 { media_type, data } => json!({
                    "type": "base64",
                    "media_type": media_type.as_str(),
                    "data": data,
                }),
            };
            Some(json!({ "type": "document", "source": source, "title": title }))
        }
        ContentBlock::Reasoning { text, signature } if supports_thinking_content => {
            let mut part = json!({ "type": "thinking", "thinking": text });
            if let Some(signature) = signature {
                part["signature"] = json!(signature);
            }
            Some(part)
        }
        ContentBlock::Reasoning { .. }
        | ContentBlock::ToolUse { .. }
        | ContentBlock::ToolResult { .. }
        | ContentBlock::RedactedReasoning { .. } => None,
    }
}

#[cfg(test)]
pub(super) fn body_for_test(config: &OpenAiConfig, request: &ModelRequest) -> Value {
    build_request(config, request)
        .expect("test config is valid")
        .body
}
