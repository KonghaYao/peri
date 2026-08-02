use serde_json::Value;

use crate::{
    ContentBlock, JsonObject, ModelError, ModelMessage, ModelResponse, ModelResult, StopReason,
    TokenUsage, ToolCall,
};

#[allow(dead_code)]
pub(super) fn decode_completed_response(value: &Value) -> ModelResult<ModelResponse> {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(provider_protocol_error)?;
    let message = choice.get("message").ok_or_else(provider_protocol_error)?;
    let (content, tool_calls) = decode_assistant_message(message)?;
    let stop_reason = stop_reason(choice.get("finish_reason").and_then(Value::as_str));
    let usage = value.get("usage").and_then(decode_usage);
    let request_id = value.get("id").and_then(Value::as_str).map(str::to_owned);
    ModelResponse::new(
        ModelMessage::assistant(content, tool_calls),
        stop_reason,
        usage,
        request_id,
    )
}

pub(super) fn decode_assistant_message(
    value: &Value,
) -> ModelResult<(Vec<ContentBlock>, Vec<ToolCall>)> {
    let mut content = Vec::new();
    let top_level_reasoning = value
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|reasoning| !reasoning.is_empty())
        .or_else(|| {
            value
                .get("reasoning")
                .and_then(Value::as_str)
                .filter(|reasoning| !reasoning.is_empty())
        });
    if let Some(reasoning) = top_level_reasoning {
        content.push(ContentBlock::reasoning(reasoning));
    }

    match value.get("content") {
        Some(Value::String(text)) if !text.is_empty() => content.push(ContentBlock::text(text)),
        Some(Value::Array(parts)) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("thinking") if top_level_reasoning.is_none() => {
                        if let Some(thinking) = part.get("thinking").and_then(Value::as_str) {
                            if !thinking.is_empty() {
                                content.push(ContentBlock::reasoning(thinking));
                            }
                        }
                    }
                    Some("text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                content.push(ContentBlock::text(text));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Some(Value::Null) | None => {}
        _ => return Err(provider_protocol_error()),
    }

    let tool_calls = value
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .map(|call| {
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(provider_protocol_error)?;
                    let function = call.get("function").ok_or_else(provider_protocol_error)?;
                    let name = function
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(provider_protocol_error)?;
                    // 缺失或空白（trim 后为空）的 arguments 等价于空对象：
                    // 部分 OpenAI-compatible 端点对无参工具返回 `"arguments": ""`。
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|arguments| !arguments.is_empty());
                    let arguments = match arguments {
                        Some(arguments) => serde_json::from_str(arguments)
                            .ok()
                            .and_then(|arguments| JsonObject::from_value(arguments).ok())
                            .ok_or_else(provider_protocol_error)?,
                        None => JsonObject::default(),
                    };
                    Ok(ToolCall::new(id, name, arguments))
                })
                .collect::<ModelResult<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok((content, tool_calls))
}

pub(super) fn decode_usage(value: &Value) -> Option<TokenUsage> {
    let input_tokens = value.get("prompt_tokens")?.as_u64()?.try_into().ok()?;
    let output_tokens = value.get("completion_tokens")?.as_u64()?.try_into().ok()?;
    let cache_read_input_tokens = value
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .and_then(|tokens| tokens.try_into().ok());
    Some(TokenUsage {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: None,
        cache_read_input_tokens,
    })
}

pub(super) fn stop_reason(value: Option<&str>) -> StopReason {
    match value.unwrap_or("stop") {
        "stop" | "end_turn" => StopReason::EndTurn,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        value => StopReason::Other {
            value: value.to_owned(),
        },
    }
}

pub(super) fn provider_protocol_error() -> ModelError {
    ModelError::protocol(crate::ProtocolErrorKind::Provider)
}
