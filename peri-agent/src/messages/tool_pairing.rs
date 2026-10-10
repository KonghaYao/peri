use std::collections::{HashMap, HashSet};

use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, ContentBlock, MessageContent, ToolCallRequest};

const MISSING_RESULT: &str = "Historical tool result is missing. Execution status and external side effects are unknown. This is a protocol placeholder, not evidence that the tool failed, was cancelled, or did not execute. Do not automatically repeat the operation; verify its effects before deciding what to do next.";

pub(crate) fn repair_model_tool_pairing(
    messages: Vec<BaseMessage>,
) -> AgentResult<Vec<BaseMessage>> {
    let result = repair(messages);
    if let Err(error) = &result {
        tracing::error!(%error, "model history tool pairing is invalid");
    }
    result
}

fn invalid(detail: impl std::fmt::Display) -> AgentError {
    AgentError::LlmError(format!("Model history tool pairing: {detail}"))
}

fn ensure_cached_call(
    calls: &mut Vec<ToolCallRequest>,
    id: &str,
    name: &str,
    input: &serde_json::Value,
) -> AgentResult<()> {
    if let Some(cached) = calls.iter().find(|call| call.id == id) {
        if cached.name != name || cached.arguments != *input {
            return Err(invalid(format!("conflicting tool call {id}")));
        }
    } else {
        calls.push(ToolCallRequest::new(id, name, input.clone()));
    }
    Ok(())
}

fn repair(mut messages: Vec<BaseMessage>) -> AgentResult<Vec<BaseMessage>> {
    let mut calls = HashMap::new();
    let mut results = HashMap::new();
    let mut inline_results = HashMap::new();
    for (index, message) in messages.iter_mut().enumerate() {
        if let BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } = message
        {
            let mut content_call_ids = HashSet::new();
            match content {
                MessageContent::Blocks(blocks) => {
                    for block in blocks {
                        if let ContentBlock::ToolUse { id, name, input } = block {
                            if !content_call_ids.insert(id.as_str()) {
                                return Err(invalid(format!("duplicate tool_use block {id}")));
                            }
                            ensure_cached_call(tool_calls, id, name, input)?;
                        }
                    }
                }
                MessageContent::Raw(blocks) => {
                    for block in blocks {
                        if block["type"] == "tool_use" {
                            let id = block["id"]
                                .as_str()
                                .ok_or_else(|| invalid("tool_use lacks ID"))?;
                            let name = block["name"]
                                .as_str()
                                .ok_or_else(|| invalid("tool_use lacks name"))?;
                            if !content_call_ids.insert(id) {
                                return Err(invalid(format!("duplicate tool_use block {id}")));
                            }
                            ensure_cached_call(tool_calls, id, name, &block["input"])?;
                        }
                    }
                }
                MessageContent::Text(_) => {}
            }
            for call in tool_calls {
                if call.id.is_empty() || calls.insert(call.id.clone(), index).is_some() {
                    return Err(invalid(format!(
                        "empty or duplicate tool call ID {}",
                        call.id
                    )));
                }
            }
        }
        if let BaseMessage::Tool { tool_call_id, .. } = message {
            if results.insert(tool_call_id.clone(), index).is_some() {
                return Err(invalid(format!("duplicate result for {tool_call_id}")));
            }
        }
        if let BaseMessage::Human {
            content: MessageContent::Blocks(blocks),
            ..
        } = message
        {
            for block in blocks {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    if inline_results.insert(tool_use_id.clone(), index).is_some() {
                        return Err(invalid(format!("duplicate result for {tool_use_id}")));
                    }
                }
            }
        }
    }
    for (tool_call_id, result_index) in &results {
        let call_index = calls
            .get(tool_call_id)
            .ok_or_else(|| invalid(format!("result has no matching call: {tool_call_id}")))?;
        if result_index <= call_index {
            return Err(invalid(format!("result precedes call: {tool_call_id}")));
        }
    }
    for (tool_call_id, result_index) in &inline_results {
        let call_index = calls
            .get(tool_call_id)
            .ok_or_else(|| invalid(format!("result has no matching call: {tool_call_id}")))?;
        if results.contains_key(tool_call_id) {
            return Err(invalid(format!("duplicate result for {tool_call_id}")));
        }
        if *result_index != call_index + 1 {
            return Err(invalid(format!(
                "inline result is not adjacent to call: {tool_call_id}"
            )));
        }
        if messages[*call_index]
            .tool_calls()
            .iter()
            .any(|call| inline_results.get(&call.id) != Some(result_index))
        {
            return Err(invalid(format!(
                "incomplete inline results for {tool_call_id}"
            )));
        }
    }

    if !inline_results.is_empty() {
        for message in &mut messages {
            if let BaseMessage::Human {
                id,
                content: MessageContent::Blocks(blocks),
            } = message
            {
                let result_count = blocks
                    .iter()
                    .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
                    .count();
                if blocks[..result_count]
                    .iter()
                    .any(|block| !matches!(block, ContentBlock::ToolResult { .. }))
                {
                    let mut result_blocks = Vec::with_capacity(result_count);
                    let mut other_blocks = Vec::with_capacity(blocks.len() - result_count);
                    for block in std::mem::take(blocks) {
                        match block {
                            ContentBlock::ToolResult { .. } => result_blocks.push(block),
                            _ => other_blocks.push(block),
                        }
                    }
                    result_blocks.extend(other_blocks);
                    *blocks = result_blocks;
                    tracing::warn!(message_id = ?id, "moving inline tool results before other user content in model view only");
                }
            }
        }
    }

    let needs_repair = calls.iter().any(|(tool_call_id, call_index)| {
        if inline_results.contains_key(tool_call_id) {
            return false;
        }
        results.get(tool_call_id).is_none_or(|result_index| {
            *result_index > call_index + messages[*call_index].tool_calls().len()
        })
    });
    if !needs_repair {
        return Ok(messages);
    }

    let mut tool_results = HashMap::with_capacity(results.len());
    let mut ordinary = Vec::with_capacity(messages.len());
    for (index, message) in messages.into_iter().enumerate() {
        match message {
            BaseMessage::Tool {
                ref tool_call_id, ..
            } => {
                tool_results.insert(tool_call_id.clone(), (index, message));
            }
            _ => ordinary.push((index, message)),
        }
    }
    let mut repaired = Vec::with_capacity(ordinary.len() + calls.len());
    for (index, message) in ordinary {
        let mut actual = Vec::new();
        let mut missing = Vec::new();
        for call in message.tool_calls() {
            if inline_results.contains_key(&call.id) {
                continue;
            }
            match tool_results.remove(&call.id) {
                Some(result) => actual.push(result),
                None => {
                    tracing::warn!(tool_call_id = %call.id, "historical tool result missing; projecting unknown-outcome placeholder without replay");
                    missing.push(BaseMessage::tool_error(&call.id, MISSING_RESULT));
                }
            }
        }
        actual.sort_by_key(|(result_index, _)| *result_index);
        let expected = index + 1..index + 1 + actual.len();
        if actual
            .iter()
            .any(|(result_index, _)| !expected.contains(result_index))
        {
            tracing::warn!(message_id = ?message.id(), "moving misplaced tool results next to their call in model view only");
        }
        repaired.push(message);
        repaired.extend(actual.into_iter().map(|(_, result)| result));
        repaired.extend(missing);
    }
    Ok(repaired)
}

#[cfg(test)]
#[path = "tool_pairing_test.rs"]
mod tests;
