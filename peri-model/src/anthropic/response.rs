#[cfg(test)]
use serde_json::Value;

use crate::{ModelError, StopReason};

#[cfg(test)]
use crate::{
    ContentBlock, JsonObject, ModelMessage, ModelResponse, ModelResult, TokenUsage, ToolCall,
};

#[cfg(test)]
pub(super) fn decode_completed_response(
    value: &Value,
    request_id: Option<String>,
) -> ModelResult<ModelResponse> {
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(provider_protocol_error)?;
    let (content, tool_calls) = decode_content_blocks(content)?;
    let stop_reason = stop_reason(value.get("stop_reason").and_then(Value::as_str));
    let usage = value.get("usage").map(decode_usage).transpose()?;
    let request_id =
        request_id.or_else(|| value.get("id").and_then(Value::as_str).map(str::to_owned));
    ModelResponse::new(
        ModelMessage::assistant(content, tool_calls),
        stop_reason,
        usage,
        request_id,
    )
}

#[cfg(test)]
pub(super) fn decode_content_blocks(
    value: &[Value],
) -> ModelResult<(Vec<ContentBlock>, Vec<ToolCall>)> {
    let mut content = Vec::new();
    let mut tool_calls = Vec::new();
    for block in value {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    content.push(ContentBlock::text(text));
                }
            }
            Some("thinking") => {
                let text = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let signature = block
                    .get("signature")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                content.push(ContentBlock::Reasoning {
                    text: text.into(),
                    signature,
                });
            }
            Some("redacted_thinking") => content.push(ContentBlock::RedactedReasoning {
                data: block.get("data").and_then(Value::as_str).map(str::to_owned),
            }),
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(provider_protocol_error)?;
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(provider_protocol_error)?;
                let arguments = block.get("input").cloned().unwrap_or(Value::Null);
                let arguments =
                    JsonObject::from_value(arguments).map_err(|_| provider_protocol_error())?;
                let tool_call = ToolCall::new(id, name, arguments);
                content.push(ContentBlock::ToolUse {
                    tool_call: tool_call.clone(),
                });
                tool_calls.push(tool_call);
            }
            _ => return Err(provider_protocol_error()),
        }
    }
    Ok((content, tool_calls))
}

#[cfg(test)]
pub(super) fn decode_usage(value: &Value) -> ModelResult<TokenUsage> {
    let input_tokens = token_count(value, "input_tokens")?;
    let output_tokens = token_count(value, "output_tokens")?;
    let cache_creation_input_tokens = optional_token_count(value, "cache_creation_input_tokens")?;
    let cache_read_input_tokens = optional_token_count(value, "cache_read_input_tokens")?;
    let input_tokens = input_tokens
        .checked_add(cache_creation_input_tokens.unwrap_or_default())
        .and_then(|tokens| tokens.checked_add(cache_read_input_tokens.unwrap_or_default()))
        .ok_or_else(provider_protocol_error)?;
    Ok(TokenUsage {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
    })
}

#[cfg(test)]
fn token_count(value: &Value, field: &str) -> ModelResult<u32> {
    optional_token_count(value, field)?.ok_or_else(provider_protocol_error)
}

#[cfg(test)]
fn optional_token_count(value: &Value, field: &str) -> ModelResult<Option<u32>> {
    value
        .get(field)
        .map(|tokens| {
            tokens
                .as_u64()
                .and_then(|tokens| u32::try_from(tokens).ok())
                .ok_or_else(provider_protocol_error)
        })
        .transpose()
}

pub(super) fn stop_reason(value: Option<&str>) -> StopReason {
    match value.unwrap_or("end_turn") {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        value => StopReason::Other {
            value: value.into(),
        },
    }
}

pub(super) fn provider_protocol_error() -> ModelError {
    ModelError::protocol(crate::ProtocolErrorKind::Provider)
}
