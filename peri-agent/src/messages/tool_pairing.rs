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
    let mut inline_result_only = HashMap::new();
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
        if let BaseMessage::Human { content, .. } = message {
            let result_only = only_inline_results(content);
            let ids = match content {
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolResult { tool_use_id, .. } => {
                            Some(Ok(tool_use_id.as_str()))
                        }
                        _ => None,
                    })
                    .collect::<AgentResult<Vec<_>>>()?,
                MessageContent::Raw(blocks) => blocks
                    .iter()
                    .filter(|block| block["type"] == "tool_result")
                    .map(|block| {
                        block["tool_use_id"]
                            .as_str()
                            .filter(|id| !id.is_empty())
                            .ok_or_else(|| invalid("raw tool_result lacks ID"))
                    })
                    .collect::<AgentResult<Vec<_>>>()?,
                MessageContent::Text(_) => Vec::new(),
            };
            if !ids.is_empty() {
                inline_result_only.insert(index, result_only);
            }
            for id in ids {
                if id.is_empty() || inline_results.insert(id.to_string(), index).is_some() {
                    return Err(invalid(format!(
                        "empty or duplicate inline result for {id}"
                    )));
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
    let mut inline_owners = HashMap::new();
    let mut checked_batches = HashSet::new();
    for (tool_call_id, result_index) in &inline_results {
        let call_index = calls
            .get(tool_call_id)
            .ok_or_else(|| invalid(format!("result has no matching call: {tool_call_id}")))?;
        if results.contains_key(tool_call_id) {
            return Err(invalid(format!("duplicate result for {tool_call_id}")));
        }
        if result_index <= call_index {
            return Err(invalid(format!("result precedes call: {tool_call_id}")));
        }
        if inline_owners
            .insert(*result_index, *call_index)
            .is_some_and(|owner| owner != *call_index)
        {
            return Err(invalid(
                "protocol_dependency: inline result message spans multiple assistant call batches",
            ));
        }
        if *result_index != call_index + 1 && !inline_result_only[result_index] {
            return Err(invalid(format!("protocol_dependency: relocating mixed inline result would reorder user content: {tool_call_id}")));
        }
        if checked_batches.insert(*call_index)
            && messages[*call_index]
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
            if let BaseMessage::Human { id, content } = message {
                let reordered = match content {
                    MessageContent::Blocks(blocks) => results_first(blocks, |block| {
                        matches!(block, ContentBlock::ToolResult { .. })
                    }),
                    MessageContent::Raw(blocks) => {
                        results_first(blocks, |block| block["type"] == "tool_result")
                    }
                    MessageContent::Text(_) => false,
                };
                if reordered {
                    tracing::warn!(message_id = ?id, "moving inline tool results before other user content in model view only");
                }
            }
        }
    }

    let needs_repair = calls.iter().any(|(tool_call_id, call_index)| {
        if let Some(result_index) = inline_results.get(tool_call_id) {
            return *result_index != call_index + 1;
        }
        results.get(tool_call_id).is_none_or(|result_index| {
            *result_index > call_index + messages[*call_index].tool_calls().len()
        })
    });
    for (tool_call_id, call_index) in &calls {
        if messages[*call_index].message_content().has_provider_state()
            && !results.contains_key(tool_call_id)
            && !inline_results.contains_key(tool_call_id)
        {
            // Rewind/fork may cut a call batch before its results. Do not pair
            // encrypted replay state with a newly invented result placeholder.
            return Err(invalid(format!(
                "incomplete provider replay unit: missing result for {tool_call_id}"
            )));
        }
    }
    if !needs_repair {
        return Ok(messages);
    }

    let mut tool_results = HashMap::with_capacity(results.len());
    let inline_positions: HashSet<usize> = inline_results.values().copied().collect();
    let mut inline_messages = HashMap::new();
    let mut ordinary = Vec::with_capacity(messages.len());
    for (index, message) in messages.into_iter().enumerate() {
        if inline_positions.contains(&index) {
            inline_messages.insert(index, message);
            continue;
        }
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
        let inline_message_index = message
            .tool_calls()
            .iter()
            .find_map(|call| inline_results.get(&call.id))
            .copied();
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
        if let Some(result_index) = inline_message_index {
            if let Some(result_message) = inline_messages.remove(&result_index) {
                repaired.push(result_message);
            }
        }
        repaired.extend(actual.into_iter().map(|(_, result)| result));
        repaired.extend(missing);
    }
    Ok(repaired)
}

fn only_inline_results(content: &MessageContent) -> bool {
    match content {
        MessageContent::Blocks(blocks) => {
            !blocks.is_empty()
                && blocks
                    .iter()
                    .all(|block| matches!(block, ContentBlock::ToolResult { .. }))
        }
        MessageContent::Raw(blocks) => {
            !blocks.is_empty() && blocks.iter().all(|block| block["type"] == "tool_result")
        }
        MessageContent::Text(_) => false,
    }
}

fn results_first<T>(blocks: &mut Vec<T>, is_result: impl Fn(&T) -> bool) -> bool {
    let result_count = blocks.iter().filter(|block| is_result(block)).count();
    if blocks[..result_count].iter().all(&is_result) {
        return false;
    }
    let (mut results, rest): (Vec<_>, Vec<_>) =
        std::mem::take(blocks).into_iter().partition(&is_result);
    results.extend(rest);
    *blocks = results;
    true
}

#[cfg(test)]
#[path = "tool_pairing_test.rs"]
mod tests;
