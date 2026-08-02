use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use serde_json::Value;

use crate::{
    runtime::stream::SseDecoderFactory, transport::SseEvent, ContentBlock, JsonObject,
    ModelMessage, ModelResponse, ModelResult, ModelStreamEvent, TokenUsage, ToolCall,
};

use super::response::{decode_usage, provider_protocol_error, stop_reason};

#[derive(Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Default)]
struct StreamState {
    text: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, ToolCallAccumulator>,
    usage: Option<TokenUsage>,
    request_id: Option<String>,
    finish_reason: Option<String>,
}

pub(super) fn decoders() -> SseDecoderFactory {
    Arc::new(|| {
        let state = Arc::new(Mutex::new(StreamState::default()));
        let decoder = {
            let state = Arc::clone(&state);
            Arc::new(move |event, _header_request_id: Option<String>| decode_event(&state, event))
        };
        let completion_decoder = Arc::new(move || complete_stream(&state));
        (decoder, completion_decoder)
    })
}

fn decode_event(state: &Mutex<StreamState>, event: SseEvent) -> ModelResult<Vec<ModelStreamEvent>> {
    if event.data == "[DONE]" {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(&event.data).map_err(|_| provider_protocol_error())?;
    let mut state = state.lock().map_err(|_| provider_protocol_error())?;
    if state.request_id.is_none() {
        state.request_id = value.get("id").and_then(Value::as_str).map(str::to_owned);
    }

    let mut events = Vec::new();
    if let Some(usage) = value.get("usage").and_then(decode_usage) {
        state.usage = Some(usage.clone());
        events.push(ModelStreamEvent::Usage(usage));
    }

    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return Ok(events);
    };
    let delta = choice.get("delta").unwrap_or(&Value::Null);

    if let Some(reasoning) = delta
        .get("reasoning_content")
        .and_then(Value::as_str)
        .or_else(|| delta.get("reasoning").and_then(Value::as_str))
        .filter(|reasoning| !reasoning.is_empty())
    {
        state.reasoning.push_str(reasoning);
        events.push(ModelStreamEvent::ReasoningDelta {
            text: reasoning.to_owned(),
        });
    }
    if let Some(text) = delta
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        state.text.push_str(text);
        events.push(ModelStreamEvent::TextDelta {
            text: text.to_owned(),
        });
    }
    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let index = tool_call
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(provider_protocol_error)?;
            let accumulator = state.tool_calls.entry(index).or_default();
            let id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let name = tool_call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let arguments_delta = tool_call
                .get("function")
                .and_then(|function| function.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Some(id) = &id {
                accumulator.id = Some(id.clone());
            }
            if let Some(name) = &name {
                accumulator.name = Some(name.clone());
            }
            accumulator.arguments.push_str(&arguments_delta);
            events.push(ModelStreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            });
        }
    }

    if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
        state.finish_reason = Some(finish_reason.to_owned());
    }
    Ok(events)
}

fn complete_stream(state: &Mutex<StreamState>) -> ModelResult<Vec<ModelStreamEvent>> {
    let state = state.lock().map_err(|_| provider_protocol_error())?;
    let finish_reason = state.finish_reason.as_deref();
    Ok(vec![ModelStreamEvent::Completed(completed_response(
        &state,
        finish_reason,
    )?)])
}

fn completed_response(
    state: &StreamState,
    finish_reason: Option<&str>,
) -> ModelResult<ModelResponse> {
    let tool_calls = state
        .tool_calls
        .values()
        .map(|tool_call| {
            let id = tool_call
                .id
                .as_deref()
                .ok_or_else(provider_protocol_error)?;
            let name = tool_call
                .name
                .as_deref()
                .ok_or_else(provider_protocol_error)?;
            let arguments: Value = serde_json::from_str(&tool_call.arguments)
                .map_err(|_| provider_protocol_error())?;
            let arguments =
                JsonObject::from_value(arguments).map_err(|_| provider_protocol_error())?;
            Ok(ToolCall::new(id, name, arguments))
        })
        .collect::<ModelResult<Vec<_>>>()?;
    let mut content = Vec::new();
    if !state.reasoning.is_empty() {
        content.push(ContentBlock::reasoning(&state.reasoning));
    }
    if !state.text.is_empty() {
        content.push(ContentBlock::text(&state.text));
    }
    ModelResponse::new(
        ModelMessage::assistant(content, tool_calls),
        stop_reason(finish_reason),
        state.usage.clone(),
        state.request_id.clone(),
    )
}
