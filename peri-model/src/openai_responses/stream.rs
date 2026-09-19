use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use serde_json::Value;
use url::Url;

use crate::{
    runtime::stream::SseDecoderFactory, transport::SseEvent, ModelError, ModelResult,
    ModelStreamEvent, TokenUsage,
};

use super::response::{decode_completed_response, decode_usage};

/// 工具参数 delta 通过 output_index 关联到 `response.output_item.*` 声明的调用身份。
#[derive(Default)]
struct StreamState {
    request_id: Option<String>,
    tool_items: BTreeMap<usize, ToolItemIdentity>,
    completed: bool,
}

#[derive(Default, Clone)]
struct ToolItemIdentity {
    call_id: Option<String>,
    name: Option<String>,
}

pub(super) fn decoders(endpoint: Url, model: String) -> SseDecoderFactory {
    Arc::new(move || {
        let state = Arc::new(Mutex::new(StreamState::default()));
        let decoder = {
            let state = Arc::clone(&state);
            let endpoint = endpoint.clone();
            let model = model.clone();
            Arc::new(move |event, header_request_id: Option<String>| {
                decode_event(&state, event, header_request_id, &endpoint, &model)
            })
        };
        let completion_decoder = Arc::new(move || complete_stream(&state));
        (decoder, completion_decoder)
    })
}

fn decode_event(
    state: &Mutex<StreamState>,
    event: SseEvent,
    header_request_id: Option<String>,
    endpoint: &Url,
    model: &str,
) -> ModelResult<Vec<ModelStreamEvent>> {
    let mut state = state.lock().map_err(|_| provider_failure())?;
    // 同一 HTTP chunk 可能包含终态后的额外事件。成功终态不可被后续内容撤销。
    if state.completed {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(&event.data).map_err(|_| provider_failure())?;
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(provider_failure)?;
    if state.request_id.is_none() {
        state.request_id = header_request_id.or_else(|| response_id(&value));
    }

    match event_type {
        "response.queued" | "response.created" | "response.in_progress" => Ok(Vec::new()),
        "response.output_item.added" | "response.output_item.done" => {
            record_output_item(&mut state, &value)?;
            Ok(Vec::new())
        }
        "response.output_text.delta" | "response.refusal.delta" => {
            Ok(vec![ModelStreamEvent::TextDelta {
                text: required_str(&value, "delta")?.to_owned(),
            }])
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            Ok(vec![ModelStreamEvent::ReasoningDelta {
                text: required_str(&value, "delta")?.to_owned(),
            }])
        }
        "response.function_call_arguments.delta" => {
            let index = output_index(&value)?;
            let identity = state.tool_items.get(&index).cloned().unwrap_or_default();
            Ok(vec![ModelStreamEvent::ToolCallDelta {
                index,
                id: identity.call_id,
                name: identity.name,
                arguments_delta: required_str(&value, "delta")?.to_owned(),
            }])
        }
        "response.completed" => {
            let response = value.get("response").ok_or_else(provider_failure)?;
            let response =
                decode_completed_response(endpoint, model, response, state.request_id.clone())?;
            state.completed = true;
            let mut events = Vec::new();
            if let Some(usage) = response.usage().cloned() {
                events.push(ModelStreamEvent::Usage(usage));
            }
            events.push(ModelStreamEvent::Completed(response));
            Ok(events)
        }
        "response.failed" => Err(terminal_failure(&value["response"]["error"])
            .with_usage(response_usage(&value["response"]))),
        "error" => Err(terminal_failure(&value).with_usage(response_usage(&value))),
        // incomplete 是稳定失败：不成功、不自动重试（截断的工具调用不可执行）。
        "response.incomplete" => {
            Err(incomplete_failure(&value["response"])
                .with_usage(response_usage(&value["response"])))
        }
        // 已由 delta 或最终 response 覆盖的元数据/收尾事件。
        "response.output_text.done"
        | "response.refusal.done"
        | "response.reasoning_summary_text.done"
        | "response.reasoning_text.done"
        | "response.reasoning_summary_part.added"
        | "response.reasoning_summary_part.done"
        | "response.content_part.added"
        | "response.content_part.done"
        | "response.function_call_arguments.done"
        | "response.output_text.annotation.added" => Ok(Vec::new()),
        // 未知事件（内置工具、未来语义）无法保真进入本地记录，显式失败。
        _ => Err(unsupported_semantics()),
    }
}

/// provider 在 SSE 结束（`[DONE]`）时没有给出 `response.completed`。
fn complete_stream(state: &Mutex<StreamState>) -> ModelResult<Vec<ModelStreamEvent>> {
    let state = state.lock().map_err(|_| provider_failure())?;
    if state.completed {
        return Ok(Vec::new());
    }
    Err(ModelError::protocol(
        crate::ProtocolErrorKind::StreamEndedWithoutCompleted,
    ))
}

fn record_output_item(state: &mut StreamState, value: &Value) -> ModelResult<()> {
    let index = output_index(value)?;
    let item = value.get("item").ok_or_else(provider_failure)?;
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return Ok(());
    }
    let identity = state.tool_items.entry(index).or_default();
    identity.call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    identity.name = item.get("name").and_then(Value::as_str).map(str::to_owned);
    Ok(())
}

fn output_index(value: &Value) -> ModelResult<usize> {
    value
        .get("output_index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(provider_failure)
}

fn required_str<'a>(value: &'a Value, field: &str) -> ModelResult<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(provider_failure)
}

fn response_id(value: &Value) -> Option<String> {
    value
        .get("response")
        .and_then(|response| response.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn terminal_failure(error: &Value) -> ModelError {
    // 只信任明确的暂态错误码，绝不将 provider 的 message/type 原文带入诊断。
    match error.get("code").and_then(Value::as_str) {
        Some("server_error" | "rate_limit_exceeded") => provider_failure(),
        _ => ModelError::protocol_with_summary(crate::ProtocolErrorKind::Other, "response_failed"),
    }
}

fn incomplete_failure(response: &Value) -> ModelError {
    let summary = match response["incomplete_details"]["reason"].as_str() {
        Some("max_output_tokens") => "incomplete.output_limit",
        Some("content_filter") => "incomplete.content_filter",
        _ => "incomplete",
    };
    ModelError::protocol_with_summary(crate::ProtocolErrorKind::Other, summary)
}

/// 非成功终态携带的 usage。
///
/// 这是已发生的真实消耗，必须随失败保留（重试成功后仍要计入总量）。形状非法时按
/// 未上报处理：不得把失败分类改写成可重试的协议错误，否则会破坏重试门禁。
fn response_usage(response: &Value) -> Option<TokenUsage> {
    decode_usage(response).ok().flatten()
}

fn provider_failure() -> ModelError {
    ModelError::protocol(crate::ProtocolErrorKind::Provider)
}

fn unsupported_semantics() -> ModelError {
    ModelError::protocol(crate::ProtocolErrorKind::Other)
}
