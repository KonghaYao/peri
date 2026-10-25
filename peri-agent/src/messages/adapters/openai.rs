use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use super::MessageAdapter;
use crate::messages::{BaseMessage, ContentBlock, ImageSource, MessageContent, ToolCallRequest};

/// OpenAI 兼容格式的消息适配器
pub struct OpenAiAdapter;

impl OpenAiAdapter {
    fn content_to_openai(content: &MessageContent) -> Value {
        match content {
            MessageContent::Text(s) => json!(s),
            MessageContent::Blocks(blocks) => {
                let parts: Vec<Value> = blocks
                    .iter()
                    .filter_map(Self::block_to_openai_part)
                    .collect();
                if parts.is_empty() {
                    json!("")
                } else {
                    Value::Array(parts)
                }
            }
            MessageContent::Raw(values) => {
                // 过滤掉 thinking/reasoning 块（OpenAI 兼容 API 不接受 content 中的 thinking 块）
                let filtered: Vec<Value> = values
                    .iter()
                    .filter(|v| {
                        let t = v["type"].as_str().unwrap_or("");
                        t != "thinking" && t != "reasoning"
                    })
                    .cloned()
                    .collect();
                if filtered.is_empty() {
                    json!("")
                } else {
                    Value::Array(filtered)
                }
            }
        }
    }

    fn block_to_openai_part(block: &ContentBlock) -> Option<Value> {
        match block {
            ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
            ContentBlock::Image { source } => {
                let image_url = match source {
                    ImageSource::Url { url } => json!({ "url": url }),
                    ImageSource::Base64 { media_type, data } => {
                        json!({ "url": format!("data:{media_type};base64,{data}") })
                    }
                };
                Some(json!({ "type": "image_url", "image_url": image_url }))
            }
            // ToolUse / ToolResult / Reasoning 不通过 content 回传
            // - ToolUse → 顶层 tool_calls 字段
            // - ToolResult → role: "tool" 消息
            // - Reasoning → 大多数 OpenAI 兼容 provider 不支持 "thinking" content type
            ContentBlock::ToolUse { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Reasoning { .. }
            | ContentBlock::ProviderState { .. } => None,
            ContentBlock::Document { source, title } => {
                let src = serde_json::to_value(source).unwrap_or_default();
                Some(json!({ "type": "document", "source": src, "title": title }))
            }
            ContentBlock::Unknown(v) => Some(v.clone()),
        }
    }
}

impl MessageAdapter for OpenAiAdapter {
    /// BaseMessage[] → OpenAI messages JSON 数组
    ///
    /// - System 消息合并为第一条 system 角色消息
    /// - Tool 消息用 role: "tool" + tool_call_id 格式
    fn from_base_messages(messages: &[BaseMessage]) -> Value {
        let mut system_parts: Vec<String> = Vec::new();
        let mut result: Vec<Value> = Vec::new();

        for m in messages {
            match m {
                BaseMessage::System { content, .. } => {
                    let t = content.text_content();
                    if !t.trim().is_empty() {
                        system_parts.push(t);
                    }
                }
                BaseMessage::Human { content, .. } => {
                    result.push(json!({
                        "role": "user",
                        "content": Self::content_to_openai(content)
                    }));
                }
                BaseMessage::Ai {
                    content,
                    tool_calls,
                    ..
                } => {
                    // 提取 reasoning 文本，回传为 reasoning_content / reasoning 顶层字段
                    let reasoning_text = content
                        .content_blocks()
                        .iter()
                        .filter_map(|b| b.as_reasoning())
                        .collect::<Vec<_>>()
                        .join("");
                    if tool_calls.is_empty() {
                        let mut msg = json!({
                            "role": "assistant",
                            "content": Self::content_to_openai(content)
                        });
                        let rv = json!(reasoning_text);
                        msg["reasoning_content"] = rv.clone();
                        msg["reasoning"] = rv;
                        result.push(msg);
                    } else {
                        let tcs: Vec<Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments.to_string()
                                    }
                                })
                            })
                            .collect();
                        let mut msg = json!({
                            "role": "assistant",
                            "content": Self::content_to_openai(content),
                            "tool_calls": tcs
                        });
                        let rv = json!(reasoning_text);
                        msg["reasoning_content"] = rv.clone();
                        msg["reasoning"] = rv;
                        result.push(msg);
                    }
                }
                BaseMessage::Tool {
                    tool_call_id,
                    content,
                    is_error,
                    ..
                } => {
                    let content_val = Self::content_to_openai(content);
                    let final_content = if *is_error {
                        let text = content_val
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| content_val.to_string());
                        json!(format!("[ERROR] {}", text))
                    } else {
                        content_val
                    };
                    result.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": final_content,
                    }));
                }
            }
        }

        if !system_parts.is_empty() {
            result.insert(
                0,
                json!({ "role": "system", "content": system_parts.join("\n\n") }),
            );
        }

        Value::Array(result)
    }

    /// OpenAI 原生 message JSON → BaseMessage
    fn to_base_message(value: &Value) -> Result<BaseMessage> {
        let role = value["role"]
            .as_str()
            .ok_or_else(|| anyhow!("缺少 role 字段"))?;
        match role {
            "user" => {
                let content = parse_openai_content(&value["content"]);
                Ok(BaseMessage::human(content))
            }
            "assistant" => {
                let content_str = value["content"].as_str().unwrap_or("").to_string();
                let mut blocks: Vec<ContentBlock> = Vec::new();

                // reasoning_content（deepseek-r1 等）/ reasoning（GLM 系列等）
                let reasoning_text = value["reasoning_content"]
                    .as_str()
                    .or_else(|| value["reasoning"].as_str());
                if let Some(reasoning) = reasoning_text {
                    if !reasoning.is_empty() {
                        blocks.push(ContentBlock::reasoning(reasoning));
                    }
                }
                if !content_str.is_empty() {
                    blocks.push(ContentBlock::text(content_str.clone()));
                }

                let tool_calls_arr = value["tool_calls"].as_array();
                if let Some(tcs_raw) = tool_calls_arr {
                    let tool_calls: Vec<ToolCallRequest> = tcs_raw
                        .iter()
                        .filter_map(|tc| {
                            let id = tc["id"].as_str()?;
                            let name = tc["function"]["name"].as_str()?;
                            let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                            let arguments = serde_json::from_str::<Value>(args_str)
                                .unwrap_or(Value::String(args_str.to_string()));
                            blocks.push(ContentBlock::tool_use(id, name, arguments.clone()));
                            Some(ToolCallRequest::new(id, name, arguments))
                        })
                        .collect();
                    let content = if blocks.len() == 1 && blocks[0].as_text().is_some() {
                        MessageContent::text(content_str)
                    } else if blocks.is_empty() {
                        MessageContent::default()
                    } else {
                        MessageContent::Blocks(blocks)
                    };
                    Ok(BaseMessage::ai_with_tool_calls(content, tool_calls))
                } else {
                    // 普通文本回复
                    Ok(BaseMessage::ai(content_str))
                }
            }
            "system" => {
                let content = parse_openai_content(&value["content"]);
                Ok(BaseMessage::system(content))
            }
            "tool" => {
                let tool_call_id = value["tool_call_id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("tool 消息缺少 tool_call_id"))?;
                let raw_content = value["content"].as_str().unwrap_or("");
                if let Some(stripped) = raw_content.strip_prefix("[ERROR] ") {
                    Ok(BaseMessage::tool_error(tool_call_id, stripped))
                } else {
                    let content = parse_openai_content(&value["content"]);
                    Ok(BaseMessage::tool_result(tool_call_id, content))
                }
            }
            other => Err(anyhow!("未知 role: {other}")),
        }
    }
}

/// 将 OpenAI content 字段（string 或 array）解析为 MessageContent
fn parse_openai_content(content: &Value) -> MessageContent {
    match content {
        Value::String(s) => MessageContent::text(s.clone()),
        Value::Array(parts) => {
            let blocks: Vec<ContentBlock> = parts
                .iter()
                .filter_map(|part| match part["type"].as_str() {
                    Some("text") => {
                        let text = part["text"].as_str().unwrap_or("").to_string();
                        Some(ContentBlock::text(text))
                    }
                    Some("image_url") => {
                        let url = part["image_url"]["url"].as_str().unwrap_or("").to_string();
                        Some(ContentBlock::image_url(url))
                    }
                    _ => None,
                })
                .collect();
            if blocks.is_empty() {
                MessageContent::text("")
            } else {
                MessageContent::Blocks(blocks)
            }
        }
        Value::Null => MessageContent::text(""),
        _ => MessageContent::text(content.to_string()),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "openai_test.rs"]
mod tests;
