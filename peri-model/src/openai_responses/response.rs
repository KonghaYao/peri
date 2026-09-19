use serde_json::Value;
use url::Url;

use crate::{
    ContentBlock, ModelError, ModelResponse, ModelResult, ResponsesHistoryV1,
    ResponsesSourceIdentity, StopReason, TokenUsage,
};

/// 原生 output 中允许进入本地记录的三类语义 item。
const SEMANTIC_ITEM_TYPES: [&str; 3] = ["message", "reasoning", "function_call"];

/// 解码 `response.completed` 的 `response` 对象。
///
/// 只有该终态会构造 [`ModelResponse`]：非 completed 状态、无法保真的 output item 与
/// 校验失败的原生记录都显式失败，绝不产出可执行的半成品。
pub(super) fn decode_completed_response(
    endpoint: &Url,
    model: &str,
    response: &Value,
    request_id: Option<String>,
) -> ModelResult<ModelResponse> {
    if response.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(unsupported_semantics());
    }
    let items = output_items(response)?;
    let source =
        ResponsesSourceIdentity::capture(endpoint, model).map_err(|_| unsupported_semantics())?;
    let history = ResponsesHistoryV1::new(source, items).map_err(|_| unsupported_semantics())?;

    let mut content = Vec::new();
    let reasoning = history.reasoning_summary_text();
    if !reasoning.is_empty() {
        content.push(ContentBlock::reasoning(reasoning));
    }
    let text = history.visible_text();
    if !text.is_empty() {
        content.push(ContentBlock::text(text));
    }
    content.extend(
        history
            .refusals()
            .into_iter()
            .filter(|refusal| !refusal.is_empty())
            .map(ContentBlock::text),
    );
    let tool_calls = history.tool_calls();
    content.push(ContentBlock::ResponsesNativeHistory {
        history: Box::new(history),
    });

    let stop_reason = if tool_calls.is_empty() {
        StopReason::EndTurn
    } else {
        StopReason::ToolUse
    };
    ModelResponse::new(
        crate::ModelMessage::assistant(content, tool_calls),
        stop_reason,
        decode_usage(response)?,
        request_id,
    )
}

fn output_items(response: &Value) -> ModelResult<Vec<Value>> {
    let items = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(malformed)?;
    items
        .iter()
        .map(|item| {
            let item_type = item.get("type").and_then(Value::as_str);
            match item_type {
                Some(item_type) if SEMANTIC_ITEM_TYPES.contains(&item_type) => Ok(item.clone()),
                // 内置工具调用（web_search/file_search/computer 等）无法进入本地记录，
                // 也就无法在续轮保真回放；显式失败而不是丢弃原生事实。
                _ => Err(unsupported_semantics()),
            }
        })
        .collect()
}

pub(super) fn decode_usage(response: &Value) -> ModelResult<Option<TokenUsage>> {
    let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) else {
        return Ok(None);
    };
    let details = usage.get("input_tokens_details");
    Ok(Some(TokenUsage {
        input_tokens: required_u32(usage, "input_tokens")?,
        output_tokens: required_u32(usage, "output_tokens")?,
        // 官方缓存字段按「写入」与「读取」分开；缺失保持缺失，显式零保持零。
        cache_creation_input_tokens: optional_u32(details, "cache_write_tokens")?,
        cache_read_input_tokens: optional_u32(details, "cached_tokens")?,
    }))
}

fn required_u32(value: &Value, field: &str) -> ModelResult<u32> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(malformed)
}

fn optional_u32(value: Option<&Value>, field: &str) -> ModelResult<Option<u32>> {
    match value.and_then(|value| value.get(field)) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .map(Some)
            .ok_or_else(malformed),
    }
}

/// provider 载荷形状与官方 schema 不符：可重试的 provider 失败。
fn malformed() -> ModelError {
    ModelError::protocol(crate::ProtocolErrorKind::Provider)
}

/// 语义不受支持：稳定失败，重试不会改变结果。
fn unsupported_semantics() -> ModelError {
    ModelError::protocol(crate::ProtocolErrorKind::Other)
}
