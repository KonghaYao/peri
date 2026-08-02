use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::{
    runtime::stream::SseDecoderFactory, transport::SseEvent, ContentBlock, JsonObject,
    ModelMessage, ModelResponse, ModelResult, ModelStreamEvent, TokenUsage, ToolCall,
};

use super::response::{provider_protocol_error, stop_reason};

#[derive(Default)]
struct StreamState {
    content: Vec<ContentBlock>,
    active: Option<ActiveBlock>,
    input_tokens: u32,
    cache_creation_input_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    output_tokens: u32,
    stop_reason: Option<String>,
    request_id: Option<String>,
    last_emitted_usage: Option<TokenUsage>,
    message_started: bool,
    message_delta_seen: bool,
    next_content_block_index: usize,
    completed: bool,
}

struct ActiveBlock {
    index: usize,
    kind: ActiveKind,
}

enum ActiveKind {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
    Redacted {
        data: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        arguments: String,
    },
}

pub(super) fn decoders() -> SseDecoderFactory {
    Arc::new(|| {
        let state = Arc::new(Mutex::new(StreamState::default()));
        let decoder = {
            let state = Arc::clone(&state);
            Arc::new(move |event, header_request_id: Option<String>| {
                decode_event(&state, event, header_request_id)
            })
        };
        let completion_decoder = Arc::new(move || Ok(Vec::new()));
        (decoder, completion_decoder)
    })
}

fn decode_event(
    state: &Mutex<StreamState>,
    event: SseEvent,
    header_request_id: Option<String>,
) -> ModelResult<Vec<ModelStreamEvent>> {
    let value: Value = serde_json::from_str(&event.data).map_err(|_| provider_protocol_error())?;
    let payload_type = match value.get("type") {
        Some(Value::String(payload_type)) => Some(payload_type.as_str()),
        Some(_) => return Err(provider_protocol_error()),
        None => None,
    };
    let event_type = match (event.event.as_deref(), payload_type) {
        (Some(event_type), Some(payload_type)) if event_type == payload_type => event_type,
        (Some(_), Some(_)) => return Err(provider_protocol_error()),
        (Some(event_type), None) | (None, Some(event_type)) => event_type,
        (None, None) => return Err(provider_protocol_error()),
    };
    let mut state = state.lock().map_err(|_| provider_protocol_error())?;
    match event_type {
        "message_start" => {
            if state.message_started || state.completed {
                return Err(provider_protocol_error());
            }
            state.message_started = true;
            if state.request_id.is_none() {
                state.request_id = header_request_id.or_else(|| extract_request_id(&value));
            }
            let message = value
                .get("message")
                .filter(|message| message.is_object())
                .ok_or_else(provider_protocol_error)?;
            if state.request_id.is_none() {
                state.request_id = extract_request_id(message);
            }
            update_input_usage(
                &mut state,
                message.get("usage").or_else(|| value.get("usage")),
            )?;
            Ok(usage_event_if_changed(&mut state)?.into_iter().collect())
        }
        "content_block_start" => {
            ensure_streaming(&state)?;
            start_block(&mut state, &value)
        }
        "content_block_delta" => {
            ensure_streaming(&state)?;
            apply_delta(&mut state, &value)
        }
        "content_block_stop" => {
            ensure_streaming(&state)?;
            finish_block(&mut state, &value)
        }
        "message_delta" => {
            ensure_streaming(&state)?;
            if state.active.is_some() || state.message_delta_seen {
                return Err(provider_protocol_error());
            }
            state.message_delta_seen = true;
            let delta = value
                .get("delta")
                .filter(|delta| delta.is_object())
                .ok_or_else(provider_protocol_error)?;
            let stop_reason = delta
                .get("stop_reason")
                .and_then(Value::as_str)
                .ok_or_else(provider_protocol_error)?;
            state.stop_reason = Some(stop_reason.into());
            update_output_usage(&mut state, value.get("usage"))?;
            Ok(usage_event_if_changed(&mut state)?.into_iter().collect())
        }
        "message_stop" => {
            ensure_streaming(&state)?;
            if state.active.is_some() || !state.message_delta_seen {
                return Err(provider_protocol_error());
            }
            update_input_usage(&mut state, value.get("usage"))?;
            update_output_usage(&mut state, value.get("usage"))?;
            state.completed = true;
            let mut events = usage_event_if_changed(&mut state)?
                .into_iter()
                .collect::<Vec<_>>();
            events.push(ModelStreamEvent::Completed(completed_response(&state)?));
            Ok(events)
        }
        "ping" if !state.completed => Ok(Vec::new()),
        _ => Err(provider_protocol_error()),
    }
}

fn ensure_streaming(state: &StreamState) -> ModelResult<()> {
    if state.message_started && !state.completed {
        Ok(())
    } else {
        Err(provider_protocol_error())
    }
}

fn start_block(state: &mut StreamState, value: &Value) -> ModelResult<Vec<ModelStreamEvent>> {
    if state.active.is_some() || state.message_delta_seen {
        return Err(provider_protocol_error());
    }
    let index = value
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(provider_protocol_error)?;
    if index != state.next_content_block_index {
        return Err(provider_protocol_error());
    }
    state.next_content_block_index = state
        .next_content_block_index
        .checked_add(1)
        .ok_or_else(provider_protocol_error)?;
    let block = value
        .get("content_block")
        .ok_or_else(provider_protocol_error)?;
    let kind = match block.get("type").and_then(Value::as_str) {
        Some("text") => ActiveKind::Text {
            text: String::new(),
        },
        Some("thinking") => ActiveKind::Thinking {
            text: String::new(),
            signature: block
                .get("signature")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        Some("redacted_thinking") => ActiveKind::Redacted {
            data: block.get("data").and_then(Value::as_str).map(str::to_owned),
        },
        Some("tool_use") => {
            let id = block
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(provider_protocol_error)?;
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(provider_protocol_error)?;
            state.active = Some(ActiveBlock {
                index,
                kind: ActiveKind::ToolUse {
                    id: id.into(),
                    name: name.into(),
                    arguments: String::new(),
                },
            });
            return Ok(vec![ModelStreamEvent::ToolCallDelta {
                index,
                id: Some(id.into()),
                name: Some(name.into()),
                arguments_delta: String::new(),
            }]);
        }
        _ => return Err(provider_protocol_error()),
    };
    state.active = Some(ActiveBlock { index, kind });
    Ok(Vec::new())
}

fn apply_delta(state: &mut StreamState, value: &Value) -> ModelResult<Vec<ModelStreamEvent>> {
    let index = value
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(provider_protocol_error)?;
    let delta = value.get("delta").ok_or_else(provider_protocol_error)?;
    let active = state.active.as_mut().ok_or_else(provider_protocol_error)?;
    if active.index != index {
        return Err(provider_protocol_error());
    }
    match (&mut active.kind, delta.get("type").and_then(Value::as_str)) {
        (ActiveKind::Text { text }, Some("text_delta")) => {
            let delta = delta
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            text.push_str(delta);
            Ok((!delta.is_empty())
                .then(|| ModelStreamEvent::TextDelta { text: delta.into() })
                .into_iter()
                .collect::<Vec<_>>())
        }
        (ActiveKind::Thinking { text, .. }, Some("thinking_delta")) => {
            let delta = delta
                .get("thinking")
                .and_then(Value::as_str)
                .unwrap_or_default();
            text.push_str(delta);
            Ok((!delta.is_empty())
                .then(|| ModelStreamEvent::ReasoningDelta { text: delta.into() })
                .into_iter()
                .collect::<Vec<_>>())
        }
        (ActiveKind::Thinking { signature, .. }, Some("signature_delta")) => {
            let delta = delta
                .get("signature")
                .and_then(Value::as_str)
                .ok_or_else(provider_protocol_error)?;
            signature.get_or_insert_with(String::new).push_str(delta);
            Ok(Vec::new())
        }
        (ActiveKind::ToolUse { arguments, .. }, Some("input_json_delta")) => {
            let delta = delta
                .get("partial_json")
                .and_then(Value::as_str)
                .unwrap_or_default();
            arguments.push_str(delta);
            Ok(vec![ModelStreamEvent::ToolCallDelta {
                index: active.index,
                id: None,
                name: None,
                arguments_delta: delta.into(),
            }])
        }
        _ => Err(provider_protocol_error()),
    }
}

fn finish_block(state: &mut StreamState, value: &Value) -> ModelResult<Vec<ModelStreamEvent>> {
    let index = value
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(provider_protocol_error)?;
    let active = state.active.take().ok_or_else(provider_protocol_error)?;
    if active.index != index {
        return Err(provider_protocol_error());
    }
    match active.kind {
        ActiveKind::Text { text } => state.content.push(ContentBlock::text(text)),
        ActiveKind::Thinking { text, signature } => state
            .content
            .push(ContentBlock::Reasoning { text, signature }),
        ActiveKind::Redacted { data } => {
            state.content.push(ContentBlock::RedactedReasoning { data })
        }
        ActiveKind::ToolUse {
            id,
            name,
            arguments,
        } => {
            let arguments: Value =
                serde_json::from_str(&arguments).map_err(|_| provider_protocol_error())?;
            let arguments =
                JsonObject::from_value(arguments).map_err(|_| provider_protocol_error())?;
            state.content.push(ContentBlock::ToolUse {
                tool_call: ToolCall::new(id, name, arguments),
            });
        }
    }
    Ok(Vec::new())
}

fn update_input_usage(state: &mut StreamState, usage: Option<&Value>) -> ModelResult<()> {
    let Some(usage) = usage else {
        return Ok(());
    };
    if state.input_tokens == 0 {
        state.input_tokens = token_count(usage, "input_tokens")?.unwrap_or_default();
    }
    if state.cache_creation_input_tokens.is_none() {
        state.cache_creation_input_tokens = token_count(usage, "cache_creation_input_tokens")?;
    }
    if state.cache_read_input_tokens.is_none() {
        state.cache_read_input_tokens = token_count(usage, "cache_read_input_tokens")?;
    }
    Ok(())
}

fn update_output_usage(state: &mut StreamState, usage: Option<&Value>) -> ModelResult<()> {
    if let Some(output_tokens) = token_count(usage.unwrap_or(&Value::Null), "output_tokens")? {
        state.output_tokens = output_tokens;
    }
    Ok(())
}

fn token_count(usage: &Value, field: &str) -> ModelResult<Option<u32>> {
    usage
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .and_then(|tokens| u32::try_from(tokens).ok())
                .ok_or_else(provider_protocol_error)
        })
        .transpose()
}

fn usage_event_if_changed(state: &mut StreamState) -> ModelResult<Option<ModelStreamEvent>> {
    let usage = current_usage(state)?;
    if state.last_emitted_usage.as_ref() == Some(&usage) {
        return Ok(None);
    }
    state.last_emitted_usage = Some(usage.clone());
    Ok(Some(ModelStreamEvent::Usage(usage)))
}

fn current_usage(state: &StreamState) -> ModelResult<TokenUsage> {
    let input_tokens = state
        .input_tokens
        .checked_add(state.cache_creation_input_tokens.unwrap_or_default())
        .and_then(|tokens| tokens.checked_add(state.cache_read_input_tokens.unwrap_or_default()))
        .ok_or_else(provider_protocol_error)?;
    Ok(TokenUsage {
        input_tokens,
        output_tokens: state.output_tokens,
        cache_creation_input_tokens: state.cache_creation_input_tokens,
        cache_read_input_tokens: state.cache_read_input_tokens,
    })
}

fn completed_response(state: &StreamState) -> ModelResult<ModelResponse> {
    let tool_calls = state
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { tool_call } => Some(tool_call.clone()),
            _ => None,
        })
        .collect();
    let usage = current_usage(state)?;
    ModelResponse::new(
        ModelMessage::assistant(state.content.clone(), tool_calls),
        stop_reason(state.stop_reason.as_deref()),
        Some(usage),
        state.request_id.clone(),
    )
}

fn extract_request_id(value: &Value) -> Option<String> {
    value.get("id").and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [回归测试] 完成后的任何 Anthropic 生命周期事件都必须被 decoder 拒绝。
    ///
    /// 历史背景：外层 stream 在 Completed 后停止消费，端到端 fixture 看不到后续 SSE；
    /// decoder 曾单独放行重复 message_stop，形成与其他完成态事件不一致的 fail-open 路径。
    #[test]
    fn completed_anthropic_decoder_rejects_repeated_message_stop() {
        let state = Mutex::new(StreamState::default());
        for (event, expected) in [
            (
                SseEvent {
                    event: Some("message_start".into()),
                    data: "{\"message\":{\"id\":\"body-id\"}}".into(),
                },
                true,
            ),
            (
                SseEvent {
                    event: Some("message_delta".into()),
                    data: "{\"delta\":{\"stop_reason\":\"end_turn\"}}".into(),
                },
                true,
            ),
            (
                SseEvent {
                    event: Some("message_stop".into()),
                    data: "{}".into(),
                },
                true,
            ),
            (
                SseEvent {
                    event: Some("message_stop".into()),
                    data: "{}".into(),
                },
                false,
            ),
        ] {
            let result = decode_event(&state, event, None);
            assert_eq!(result.is_ok(), expected);
        }
    }
}
