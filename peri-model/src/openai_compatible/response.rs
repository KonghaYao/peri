use serde_json::Value;

use crate::{ModelError, StopReason, TokenUsage};

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

#[track_caller]
pub(super) fn provider_protocol_error() -> ModelError {
    ModelError::protocol(crate::ProtocolErrorKind::Provider).with_message(format!(
        "invalid provider payload at {}",
        std::panic::Location::caller()
    ))
}
