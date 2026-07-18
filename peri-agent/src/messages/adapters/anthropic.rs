use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use super::MessageAdapter;
use crate::messages::{BaseMessage, ContentBlock, ImageSource, MessageContent, ToolCallRequest};

/// Anthropic Messages API 格式的消息适配器
pub struct AnthropicAdapter;

impl AnthropicAdapter {
    fn block_to_anthropic(block: &ContentBlock) -> Option<Value> {
        match block {
            ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
            ContentBlock::Image { source } => match source {
                ImageSource::Base64 { media_type, data } => Some(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": media_type,
                        "data": data
                    }
                })),
                ImageSource::Url { url } => Some(json!({
                    "type": "image",
                    "source": { "type": "url", "url": url }
                })),
            },
            ContentBlock::Document { source, title } => {
                let src = serde_json::to_value(source).unwrap_or_default();
                let mut obj = json!({ "type": "document", "source": src });
                if let Some(t) = title {
                    obj["title"] = json!(t);
                }
                Some(obj)
            }
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input
            })),
            ContentBlock::ToolResult {
                id: block_id,
                tool_use_id,
                content,
                is_error,
            } => {
                let content_val: Vec<Value> = content
                    .iter()
                    .filter_map(Self::block_to_anthropic)
                    .collect();
                // 部分 provider（如 GLM Anthropic 兼容端口）要求 tool_result block 含 id 字段
                let bid = block_id
                    .clone()
                    .unwrap_or_else(|| format!("toolu_{}", uuid::Uuid::now_v7()));
                Some(json!({
                    "type": "tool_result",
                    "id": bid,
                    "tool_use_id": tool_use_id,
                    "content": content_val,
                    "is_error": is_error
                }))
            }
            ContentBlock::Reasoning { text, signature } => {
                let mut obj = json!({ "type": "thinking", "thinking": text });
                if let Some(sig) = signature {
                    obj["signature"] = json!(sig);
                }
                Some(obj)
            }
            ContentBlock::Unknown(v) => Some(v.clone()),
        }
    }

    fn content_to_anthropic(content: &MessageContent) -> Value {
        match content {
            MessageContent::Text(s) => json!(s),
            MessageContent::Blocks(blocks) => {
                let parts: Vec<Value> =
                    blocks.iter().filter_map(Self::block_to_anthropic).collect();
                Value::Array(parts)
            }
            MessageContent::Raw(values) => Value::Array(values.clone()),
        }
    }

    fn parse_content_blocks(raw_blocks: &[Value]) -> (Vec<ContentBlock>, Vec<ToolCallRequest>) {
        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut tool_calls: Vec<ToolCallRequest> = Vec::new();

        for b in raw_blocks {
            match b["type"].as_str() {
                Some("text") => {
                    if let Some(text) = b["text"].as_str() {
                        blocks.push(ContentBlock::text(text));
                    }
                }
                Some("thinking") => {
                    let text = b["thinking"].as_str().unwrap_or("").to_string();
                    let signature = b["signature"].as_str().map(|s| s.to_string());
                    if let Some(sig) = signature {
                        blocks.push(ContentBlock::reasoning_with_signature(text, sig));
                    } else {
                        blocks.push(ContentBlock::reasoning(text));
                    }
                }
                Some("tool_use") => {
                    let id = b["id"].as_str().unwrap_or("").to_string();
                    let name = b["name"].as_str().unwrap_or("").to_string();
                    let input = b["input"].clone();
                    blocks.push(ContentBlock::tool_use(&id, &name, input.clone()));
                    tool_calls.push(ToolCallRequest::new(id, name, input));
                }
                _ => {
                    blocks.push(ContentBlock::Unknown(b.clone()));
                }
            }
        }
        (blocks, tool_calls)
    }
}

impl MessageAdapter for AnthropicAdapter {
    /// BaseMessage[] → Anthropic messages JSON 数组
    ///
    /// - System 消息提取为第一条（系统角色），实际上 Anthropic 的 system 字段需要单独提取，
    ///   但本适配器将其作为 system role 消息插入（与 OpenAI 格式兼容），
    ///   使用 `from_base_messages_with_system` 可同时获得 system 字符串
    /// - Tool 消息合并到前一条 user 消息的 content blocks
    fn from_base_messages(messages: &[BaseMessage]) -> Value {
        let (msgs, _system) = Self::to_anthropic_with_system(messages);
        Value::Array(msgs)
    }

    /// Anthropic 原生 message JSON → BaseMessage
    fn to_base_message(value: &Value) -> Result<BaseMessage> {
        let role = value["role"]
            .as_str()
            .ok_or_else(|| anyhow!("缺少 role 字段"))?;
        match role {
            "user" => {
                let content = parse_anthropic_content(&value["content"]);
                Ok(BaseMessage::human(content))
            }
            "assistant" => match &value["content"] {
                Value::String(s) => Ok(BaseMessage::ai(s.clone())),
                Value::Array(blocks) => {
                    let (content_blocks, tool_calls) = Self::parse_content_blocks(blocks);
                    if tool_calls.is_empty() {
                        if content_blocks.len() == 1 {
                            if let ContentBlock::Text { text } = &content_blocks[0] {
                                return Ok(BaseMessage::ai(text.clone()));
                            }
                        }
                        Ok(BaseMessage::ai(MessageContent::Blocks(content_blocks)))
                    } else {
                        Ok(BaseMessage::ai_with_tool_calls(
                            MessageContent::Blocks(content_blocks),
                            tool_calls,
                        ))
                    }
                }
                _ => Ok(BaseMessage::ai("")),
            },
            "system" => {
                let content = parse_anthropic_content(&value["content"]);
                Ok(BaseMessage::system(content))
            }
            other => Err(anyhow!("未知 role: {other}")),
        }
    }
}

impl AnthropicAdapter {
    /// 返回 (messages_array, system_text)
    /// system_text 可用于 Anthropic API 的顶层 system 字段
    pub fn to_anthropic_with_system(messages: &[BaseMessage]) -> (Vec<Value>, Option<String>) {
        let mut system_parts: Vec<String> = Vec::new();
        let mut result: Vec<Value> = Vec::new();

        for msg in messages {
            match msg {
                BaseMessage::System { content, .. } => {
                    let text = content.text_content();
                    if !text.trim().is_empty() {
                        system_parts.push(text);
                    }
                }
                BaseMessage::Human { content, .. } => {
                    result.push(json!({
                        "role": "user",
                        "content": Self::content_to_anthropic(content)
                    }));
                }
                BaseMessage::Ai {
                    content,
                    tool_calls,
                    ..
                } => {
                    if tool_calls.is_empty() {
                        result.push(json!({
                            "role": "assistant",
                            "content": Self::content_to_anthropic(content)
                        }));
                    } else {
                        let content_val = match content {
                            MessageContent::Blocks(_) | MessageContent::Raw(_) => {
                                Self::content_to_anthropic(content)
                            }
                            MessageContent::Text(t) => {
                                let mut blocks: Vec<Value> = Vec::new();
                                if !t.is_empty() {
                                    blocks.push(json!({ "type": "text", "text": t }));
                                }
                                for tc in tool_calls {
                                    blocks.push(json!({
                                        "type": "tool_use",
                                        "id": tc.id,
                                        "name": tc.name,
                                        "input": tc.arguments
                                    }));
                                }
                                Value::Array(blocks)
                            }
                        };
                        result.push(json!({ "role": "assistant", "content": content_val }));
                    }
                }
                BaseMessage::Tool {
                    id: msg_id,
                    tool_call_id,
                    content,
                    is_error,
                } => {
                    let block_id = msg_id.as_uuid().to_string();
                    let tool_result_block = json!({
                        "type": "tool_result",
                        "id": block_id,
                        "tool_use_id": tool_call_id,
                        "content": Self::content_to_anthropic(content),
                        "is_error": is_error
                    });

                    let should_append = result
                        .last()
                        .map(|last| last["role"] == "user" && last["content"].is_array())
                        .unwrap_or(false);

                    if should_append {
                        if let Some(last) = result.last_mut() {
                            if let Some(arr) = last["content"].as_array_mut() {
                                arr.push(tool_result_block);
                            }
                        }
                    } else {
                        result.push(json!({
                            "role": "user",
                            "content": [tool_result_block]
                        }));
                    }
                }
            }
        }

        let system_text = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };
        (result, system_text)
    }
}

fn parse_anthropic_content(content: &Value) -> MessageContent {
    match content {
        Value::String(s) => MessageContent::text(s.clone()),
        Value::Array(blocks) => {
            let parsed: Vec<ContentBlock> = blocks
                .iter()
                .filter_map(|b| match b["type"].as_str() {
                    Some("text") => Some(ContentBlock::text(b["text"].as_str().unwrap_or(""))),
                    Some("tool_result") => Some(ContentBlock::Unknown(b.clone())),
                    _ => None,
                })
                .collect();
            if parsed.is_empty() {
                MessageContent::text("")
            } else {
                MessageContent::Blocks(parsed)
            }
        }
        Value::Null => MessageContent::text(""),
        _ => MessageContent::text(content.to_string()),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "anthropic_test.rs"]
mod tests;
