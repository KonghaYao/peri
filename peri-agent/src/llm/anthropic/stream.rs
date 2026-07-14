use futures::StreamExt;
use serde_json::{json, Value};

use super::invoke::{build_request_body, parse_content_blocks};
use crate::{
    agent::events::ExecutorEvent,
    error::{AgentError, AgentResult},
    llm::{
        sse::SseParser,
        types::{LlmResponse, StopReason, StreamingContext},
    },
    messages::{BaseMessage, MessageContent},
};

/// Anthropic SSE 流式处理
///
/// 从 `invoke_streaming()` 中提取的流式解析逻辑，
/// 负责发送请求、解析 SSE 事件流、构建最终响应。
pub(super) async fn do_invoke_streaming(
    adapter: &super::ChatAnthropic,
    request: crate::llm::types::LlmRequest,
    ctx: StreamingContext,
) -> AgentResult<LlmResponse> {
    let msg_count = request.messages.len();
    let start = std::time::Instant::now();

    let body = build_request_body(adapter, &request, true);

    let chat_url = match &adapter.base_url {
        Some(base) => format!("{}/v1/messages", base.trim_end_matches('/')),
        None => "https://api.anthropic.com/v1/messages".to_string(),
    };

    let mut req = adapter
        .client
        .post(chat_url)
        .header("x-api-key", &adapter.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");

    if adapter.enable_cache {
        req = req.header("anthropic-beta", "prompt-caching-2024-07-31");
    }

    if let Some(ref sid) = request.session_id {
        req = req.header("x-session-id", sid.as_str());
    }

    let resp = req.json(&body).send().await.map_err(|e| {
        tracing::error!(
            provider = "anthropic", model = %adapter.model,
            elapsed_ms = start.elapsed().as_millis() as u64, error = %e,
            "LLM 流式网络请求失败"
        );
        AgentError::LlmError(e.to_string())
    })?;

    let status = resp.status();
    if !status.is_success() {
        let resp_text = resp.text().await.unwrap_or_default();
        let error_msg = serde_json::from_str::<Value>(&resp_text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "未知错误".to_string());
        tracing::error!(
            provider = "anthropic", model = %adapter.model, status = %status,
            error_message = %error_msg,
            elapsed_ms = start.elapsed().as_millis() as u64, msg_count,
            "LLM 流式 API 错误"
        );
        return Err(AgentError::LlmHttpError {
            status: status.as_u16(),
            message: format!("API 错误 {status}: {error_msg}"),
        });
    }

    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new();

    // Accumulators
    let mut text_content = String::new();
    let mut reasoning_content = String::new();
    let mut thinking_signature: Option<String> = None;
    let mut tool_use_id: Option<String> = None;
    let mut tool_use_name: Option<String> = None;
    let mut tool_input_fragments: String = String::new();
    let mut accumulated_blocks: Vec<Value> = Vec::new();
    let mut current_block_type: Option<String> = None;

    let mut input_tokens: u32 = 0;
    let mut cache_creation_input_tokens: u32 = 0;
    let mut cache_read_input_tokens: u32 = 0;
    let mut output_tokens: u32 = 0;
    let mut stop_reason_str: String = "end_turn".to_string();
    let mut stream_request_id: Option<String> = None;

    loop {
        // 在接收每个 SSE chunk 前检查取消（支持 Ctrl+C 中断长时间 LLM 调用）
        let chunk = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                tracing::info!(
                    provider = "anthropic",
                    model = %adapter.model,
                    "LLM streaming cancelled by user"
                );
                return Err(AgentError::Interrupted);
            }
            result = stream.next() => {
                match result {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Err(AgentError::LlmError(format!("流式读取失败: {e}"))),
                    None => break,
                }
            }
        };

        for (event_type, data) in parser.push(&chunk) {
            let event = event_type.as_deref().unwrap_or("");
            let parsed: Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            match event {
                "message_start" => {
                    stream_request_id = parsed["message"]["id"].as_str().map(|s| s.to_string());
                    let usage_obj = if parsed["message"]["usage"].is_object() {
                        &parsed["message"]["usage"]
                    } else if parsed["usage"].is_object() {
                        &parsed["usage"]
                    } else {
                        &serde_json::Value::Null
                    };
                    input_tokens = usage_obj["input_tokens"].as_u64().unwrap_or(0) as u32;
                    cache_creation_input_tokens = usage_obj["cache_creation_input_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32;
                    cache_read_input_tokens =
                        usage_obj["cache_read_input_tokens"].as_u64().unwrap_or(0) as u32;
                }
                "content_block_start" => {
                    let cb = &parsed["content_block"];
                    let cb_type = cb["type"].as_str().unwrap_or("");
                    current_block_type = Some(cb_type.to_string());

                    match cb_type {
                        "thinking" => {
                            if let Some(sig) = cb["signature"].as_str() {
                                thinking_signature = Some(sig.to_string());
                            }
                        }
                        "tool_use" => {
                            tool_use_id = cb["id"].as_str().map(|s| s.to_string());
                            tool_use_name = cb["name"].as_str().map(|s| s.to_string());
                            tool_input_fragments.clear();
                        }
                        _ => {}
                    }
                }
                "content_block_delta" => {
                    let delta = &parsed["delta"];
                    match delta["type"].as_str().unwrap_or("") {
                        "thinking_delta" => {
                            if let Some(t) = delta["thinking"].as_str() {
                                if !t.is_empty() {
                                    ctx.event_handler.on_event(ExecutorEvent::AiReasoning {
                                        text: t.to_string(),
                                        source_agent_id: None,
                                    });
                                    reasoning_content.push_str(t);
                                }
                            }
                        }
                        "text_delta" => {
                            if let Some(t) = delta["text"].as_str() {
                                if !t.is_empty() {
                                    ctx.event_handler.on_event(ExecutorEvent::TextChunk {
                                        message_id: ctx.message_id,
                                        chunk: t.to_string(),
                                        source_agent_id: None,
                                    });
                                    text_content.push_str(t);
                                }
                            }
                        }
                        "input_json_delta" => {
                            if let Some(json_part) = delta["partial_json"].as_str() {
                                tool_input_fragments.push_str(json_part);
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    match current_block_type.as_deref() {
                        Some("thinking") => {
                            let mut block = json!({
                                "type": "thinking",
                                "thinking": &reasoning_content
                            });
                            if let Some(ref sig) = thinking_signature {
                                block["signature"] = json!(sig);
                            }
                            accumulated_blocks.push(block);
                        }
                        Some("text") => {
                            accumulated_blocks.push(json!({
                                "type": "text",
                                "text": &text_content
                            }));
                        }
                        Some("tool_use") => {
                            let input: Value =
                                serde_json::from_str(&tool_input_fragments).unwrap_or(Value::Null);
                            accumulated_blocks.push(json!({
                                "type": "tool_use",
                                "id": tool_use_id,
                                "name": tool_use_name,
                                "input": input
                            }));
                        }
                        _ => {}
                    }
                    current_block_type = None;
                }
                "message_delta" => {
                    stop_reason_str = parsed["delta"]["stop_reason"]
                        .as_str()
                        .unwrap_or("end_turn")
                        .to_string();
                    output_tokens = parsed["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;
                    // 部分代理不发送 message_start 事件，将 input_tokens 放在此处
                    if input_tokens == 0 {
                        input_tokens = parsed["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
                        cache_creation_input_tokens = parsed["usage"]["cache_creation_input_tokens"]
                            .as_u64()
                            .unwrap_or(0)
                            as u32;
                        cache_read_input_tokens = parsed["usage"]["cache_read_input_tokens"]
                            .as_u64()
                            .unwrap_or(0) as u32;
                    }
                }
                "message_stop" if input_tokens == 0 => {
                    input_tokens = parsed["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
                    cache_creation_input_tokens = parsed["usage"]["cache_creation_input_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32;
                    cache_read_input_tokens = parsed["usage"]["cache_read_input_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32;
                }
                _ => {}
            }
        }

        if parser.is_done() {
            break;
        }
    }

    // Build final response using parse_content_blocks
    let stop_reason = StopReason::from_display(&stop_reason_str);
    let (blocks, tool_calls) = parse_content_blocks(&accumulated_blocks);

    let message = if !tool_calls.is_empty() {
        let content = if let [single] = blocks.as_slice() {
            if let Some(text) = single.as_text() {
                MessageContent::text(text)
            } else {
                MessageContent::Blocks(blocks)
            }
        } else {
            MessageContent::Blocks(blocks)
        };
        BaseMessage::ai_with_tool_calls(content, tool_calls)
    } else if let [single] = blocks.as_slice() {
        if let Some(text) = single.as_text() {
            BaseMessage::ai(text)
        } else {
            BaseMessage::ai(MessageContent::Blocks(blocks))
        }
    } else if blocks.is_empty() {
        BaseMessage::ai("")
    } else {
        BaseMessage::ai(MessageContent::Blocks(blocks))
    };

    // 规范化 input_tokens：Anthropic 的 input_tokens 不含缓存 token，
    // 加上 cache_creation + cache_read 使其与 OpenAI 语义一致（总输入）。
    let normalized_input = input_tokens + cache_creation_input_tokens + cache_read_input_tokens;

    let usage = Some(crate::llm::types::TokenUsage {
        input_tokens: normalized_input,
        output_tokens,
        cache_creation_input_tokens: Some(cache_creation_input_tokens),
        cache_read_input_tokens: Some(cache_read_input_tokens),
        request_id: stream_request_id.clone(),
    });

    tracing::info!(
        provider = "anthropic",
        model = %adapter.model,
        elapsed_ms = start.elapsed().as_millis() as u64,
        msg_count,
        input_tokens = normalized_input,
        output_tokens,
        cache_read = cache_read_input_tokens,
        cache_creation = cache_creation_input_tokens,
        "LLM streaming completed"
    );

    Ok(LlmResponse {
        message,
        stop_reason,
        usage,
        request_id: stream_request_id,
    })
}
