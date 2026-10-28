use serde_json::{json, Value};

use crate::{
    prompt_cache::strip_system_prompt_dynamic_boundaries, ContentBlock, DocumentSource,
    ImageSource, ModelMessage, ModelResult, ToolCall, ToolResult,
};

use super::{
    replay::{self, ReplayBinding},
    request::{invalid, valid_tool_name},
    ResponsesProfile, TOOL_NAMESPACE,
};

pub(super) fn messages_to_input(
    messages: &[ModelMessage],
    binding: &ReplayBinding,
) -> ModelResult<Vec<Value>> {
    let mut input = Vec::new();
    for message in messages {
        match message {
            ModelMessage::System { content } => {
                let mut parts = Vec::new();
                for block in content {
                    match block {
                        ContentBlock::Text { text } => parts.push(json!({"type": "input_text", "text": strip_system_prompt_dynamic_boundaries(text)})),
                        _ => return Err(invalid("Responses developer messages only support text")),
                    }
                }
                if !parts.is_empty() {
                    input.push(json!({"type": "message", "role": "developer", "content": parts}));
                }
            }
            ModelMessage::User { content } => {
                let parts = input_content(content)?;
                if !parts.is_empty() {
                    input.push(json!({"type": "message", "role": "user", "content": parts}));
                }
            }
            ModelMessage::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(items) = replay::replay_items(content, tool_calls, binding)? {
                    input.extend(items);
                    continue;
                }
                let mut parts = Vec::new();
                let mut embedded_calls = Vec::new();
                for block in content {
                    match block {
                        ContentBlock::Text { text } => {
                            parts.push(json!({"type": "output_text", "text": text}))
                        }
                        // Summaries/signatures from a foreign provider are not native
                        // reasoning items. Preserve their readable projection as text.
                        ContentBlock::Reasoning { text, .. } if !text.is_empty() => {
                            parts.push(json!({"type": "output_text", "text": text}))
                        }
                        ContentBlock::Reasoning { .. }
                        | ContentBlock::RedactedReasoning { .. }
                        | ContentBlock::ProviderState { .. } => {}
                        ContentBlock::ToolUse { tool_call } => {
                            embedded_calls.push(tool_call.clone())
                        }
                        _ => {
                            return Err(invalid(
                                "Responses assistant history supports text, reasoning and function calls only",
                            ));
                        }
                    }
                }
                if !parts.is_empty() {
                    input.push(json!({"type": "message", "role": "assistant", "content": parts}));
                }
                let calls = if tool_calls.is_empty() {
                    &embedded_calls
                } else {
                    if !embedded_calls.is_empty() && embedded_calls != *tool_calls {
                        return Err(invalid("Assistant content and tool_calls disagree"));
                    }
                    tool_calls
                };
                for call in calls {
                    input.push(function_call(call, binding.profile)?);
                }
            }
            ModelMessage::ToolResult { result } => input.push(function_output(result)?),
        }
    }
    Ok(input)
}

fn input_content(content: &[ContentBlock]) -> ModelResult<Vec<Value>> {
    content.iter().map(|block| match block {
        ContentBlock::Text { text } => Ok(json!({"type": "input_text", "text": text})),
        ContentBlock::Image { source } => {
            let url = match source {
                ImageSource::Url { url } => url.clone(),
                ImageSource::Base64 { media_type, data } => format!("data:{};base64,{data}", media_type.as_str()),
            };
            Ok(json!({"type": "input_image", "image_url": url}))
        }
        ContentBlock::Document { source, title } => match source {
            DocumentSource::Url { url } => Ok(json!({"type": "input_file", "file_url": url})),
            DocumentSource::Base64 { media_type, data } => Ok(json!({"type": "input_file", "filename": title.as_deref().unwrap_or("document"), "file_data": format!("data:{};base64,{data}", media_type.as_str())})),
            DocumentSource::Text { text } => Ok(json!({"type": "input_text", "text": text})),
        },
        _ => Err(invalid("Responses user/tool content contains an unsupported block")),
    }).collect()
}

fn function_call(call: &ToolCall, profile: ResponsesProfile) -> ModelResult<Value> {
    if call.id().is_empty() || !valid_tool_name(call.name()) {
        return Err(invalid("Invalid function call identity in history"));
    }
    let mut item = json!({"type": "function_call", "call_id": call.id(), "name": call.name(), "arguments": serde_json::to_string(call.arguments()).map_err(|_| invalid("Cannot serialize function arguments"))?});
    if profile == ResponsesProfile::ChatGptPlan {
        item["namespace"] = json!(TOOL_NAMESPACE);
    }
    Ok(item)
}

fn function_output(result: &ToolResult) -> ModelResult<Value> {
    if result.tool_call_id.is_empty() {
        return Err(invalid("Function result is missing call_id"));
    }
    let mut content = input_content(&result.content)?;
    if result.is_error {
        content.insert(
            0,
            json!({"type": "input_text", "text": "Tool execution failed (is_error=true)."}),
        );
    }
    Ok(json!({"type": "function_call_output", "call_id": result.tool_call_id, "output": content}))
}
