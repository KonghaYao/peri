use serde_json::{json, Value};

use crate::prompt_cache::{strip_system_prompt_dynamic_boundaries, SYSTEM_PROMPT_DYNAMIC_BOUNDARY};

pub(super) struct SystemPromptBlock {
    pub(super) text: String,
    pub(super) cache_control: bool,
}

pub(super) struct SplitSystemPrompt {
    pub(super) blocks: Vec<SystemPromptBlock>,
    pub(super) allow_fallback_cache: bool,
}

pub(super) fn split_system_blocks(text: &str) -> SplitSystemPrompt {
    if text.is_empty() {
        return SplitSystemPrompt {
            blocks: Vec::new(),
            allow_fallback_cache: false,
        };
    }
    let boundary_count = text.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count();
    if boundary_count == 1 {
        let index = text
            .find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
            .expect("count established one boundary");
        let static_text = text[..index].trim();
        let dynamic_text = text[index + SYSTEM_PROMPT_DYNAMIC_BOUNDARY.len()..].trim();
        let mut blocks = Vec::new();
        if !static_text.is_empty() {
            blocks.push(SystemPromptBlock {
                text: static_text.into(),
                cache_control: true,
            });
        }
        if !dynamic_text.is_empty() {
            blocks.push(SystemPromptBlock {
                text: dynamic_text.into(),
                cache_control: false,
            });
        }
        SplitSystemPrompt {
            blocks,
            allow_fallback_cache: false,
        }
    } else if boundary_count > 1 {
        let text = strip_system_prompt_dynamic_boundaries(text);
        let text = text.trim();
        SplitSystemPrompt {
            blocks: (!text.is_empty())
                .then(|| SystemPromptBlock {
                    text: text.into(),
                    cache_control: false,
                })
                .into_iter()
                .collect(),
            allow_fallback_cache: false,
        }
    } else {
        SplitSystemPrompt {
            blocks: vec![SystemPromptBlock {
                text: text.into(),
                cache_control: false,
            }],
            allow_fallback_cache: true,
        }
    }
}

pub(super) fn system_blocks_to_json(
    blocks: &[SystemPromptBlock],
    allow_fallback_cache: bool,
) -> Vec<Value> {
    let has_cached = blocks.iter().any(|block| block.cache_control);
    let last_index = blocks.len().saturating_sub(1);
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let mut value = json!({ "type": "text", "text": block.text });
            if block.cache_control || (allow_fallback_cache && index == last_index && !has_cached) {
                value["cache_control"] = json!({ "type": "ephemeral" });
            }
            value
        })
        .collect()
}

pub(super) fn apply_cache_to_messages(messages: &mut [Value]) {
    let user_indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message["role"] == "user").then_some(index))
        .collect::<Vec<_>>();
    if user_indices.is_empty() {
        return;
    }

    let mut target_indices = vec![user_indices[0]];
    if let Some(&last) = user_indices.last().filter(|&&last| last != user_indices[0]) {
        target_indices.push(last);
    }
    if user_indices.len() >= 3 {
        let second_to_last = user_indices[user_indices.len() - 2];
        if !target_indices.contains(&second_to_last) {
            target_indices.push(second_to_last);
        }
    }
    target_indices.sort_unstable();

    for target_index in target_indices.iter().copied() {
        let effective_index = if has_cacheable_block(&messages[target_index]) {
            Some(target_index)
        } else {
            user_indices.iter().rev().find_map(|&index| {
                (index < target_index
                    && has_cacheable_block(&messages[index])
                    && !target_indices.contains(&index))
                .then_some(index)
            })
        };
        if let Some(index) = effective_index {
            apply_cache_to_message(&mut messages[index]);
        }
    }
}

fn has_cacheable_block(message: &Value) -> bool {
    match message.get("content") {
        Some(Value::Array(blocks)) => blocks.iter().any(is_cacheable_block),
        Some(Value::String(text)) => !text.trim().is_empty(),
        _ => false,
    }
}

fn is_cacheable_block(block: &Value) -> bool {
    match block["type"].as_str() {
        Some("text") => block["text"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty()),
        Some("tool_result") => true,
        _ => false,
    }
}

fn apply_cache_to_message(message: &mut Value) {
    let Some(content) = message.get_mut("content") else {
        return;
    };
    match content {
        Value::Array(blocks) => {
            if let Some(block) = blocks.iter_mut().rfind(|block| is_cacheable_block(block)) {
                block["cache_control"] = json!({ "type": "ephemeral" });
            }
        }
        Value::String(text) if !text.trim().is_empty() => {
            *content = json!([{
                "type": "text",
                "text": text,
                "cache_control": { "type": "ephemeral" },
            }]);
        }
        _ => {}
    }
}

pub(super) fn ensure_thinking_blocks(messages: &mut [Value]) {
    for message in messages {
        if message["role"] != "assistant" || has_thinking_block(message) {
            continue;
        }
        let placeholder = json!({
            "type": "thinking",
            "thinking": "",
            "signature": "",
        });
        match message.get_mut("content") {
            Some(Value::Array(blocks)) => blocks.insert(0, placeholder),
            Some(content) => {
                let old = content.take();
                *content = Value::Array(vec![placeholder, old]);
            }
            None => message["content"] = Value::Array(vec![placeholder]),
        }
    }
}

fn has_thinking_block(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                matches!(
                    block["type"].as_str(),
                    Some("thinking" | "redacted_thinking")
                )
            })
        })
}
