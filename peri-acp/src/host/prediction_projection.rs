use std::collections::{BTreeMap, BTreeSet};

use peri_acp_types::messages::BaseMessage;

pub(super) const PREDICTION_HISTORY_WINDOW: usize = 10;

/// 投影 Prediction 上下文，并把 tool-call batch 作为原子组处理。
///
/// 窗口采用软上限：若最近消息中的 Tool result 对应窗口外的 Ai tool_calls，
/// 则向前扩展并保留该 Ai 声明及同批全部结果。历史本身不完整时，整组丢弃；
/// 孤立 Tool result 也丢弃。最终的 model bridge 仍负责请求前的契约校验。
pub(super) fn project_prediction_history(history: &[BaseMessage]) -> Vec<BaseMessage> {
    let messages: Vec<&BaseMessage> = history
        .iter()
        .filter(|message| !message.is_system())
        .collect();
    let mut selected: BTreeSet<usize> =
        (messages.len().saturating_sub(PREDICTION_HISTORY_WINDOW)..messages.len()).collect();

    let mut latest_declarations: BTreeMap<&str, usize> = BTreeMap::new();
    let mut result_owners: BTreeMap<usize, usize> = BTreeMap::new();
    let mut batch_results: BTreeMap<usize, BTreeMap<&str, usize>> = BTreeMap::new();
    for (index, message) in messages.iter().enumerate() {
        if message.has_tool_calls() {
            for call in message.tool_calls() {
                latest_declarations.insert(call.id.as_str(), index);
            }
        }
        if let BaseMessage::Tool { tool_call_id, .. } = message {
            if let Some(&ai_index) = latest_declarations.get(tool_call_id.as_str()) {
                result_owners.insert(index, ai_index);
                batch_results
                    .entry(ai_index)
                    .or_default()
                    .insert(tool_call_id.as_str(), index);
            }
        }
    }

    // A selected result pulls in the whole parallel-call batch. A Tool result may
    // only pair with the nearest declaration before it; a later duplicate ID must
    // never claim an earlier result and produce Tool-before-Ai provider history.
    loop {
        let before = selected.len();
        for index in selected.clone() {
            let Some(&ai_index) = result_owners.get(&index) else {
                continue;
            };
            selected.insert(ai_index);
            if let Some(results) = batch_results.get(&ai_index) {
                selected.extend(results.values().copied());
            }
        }
        if selected.len() == before {
            break;
        }
    }

    // Keep only complete batches. This also removes orphan results at a malformed boundary.
    let complete_ai: BTreeSet<usize> = selected
        .iter()
        .copied()
        .filter(|&index| {
            let calls = messages[index].tool_calls();
            !calls.is_empty()
                && batch_results.get(&index).is_some_and(|results| {
                    calls
                        .iter()
                        .all(|call| results.contains_key(call.id.as_str()))
                })
        })
        .collect();

    selected
        .into_iter()
        .filter(|&index| match messages[index] {
            BaseMessage::Ai { .. } if messages[index].has_tool_calls() => {
                complete_ai.contains(&index)
            }
            BaseMessage::Tool { .. } => result_owners
                .get(&index)
                .is_some_and(|ai_index| complete_ai.contains(ai_index)),
            _ => true,
        })
        .map(|index| messages[index].clone())
        .collect()
}

#[cfg(test)]
#[path = "prediction_projection_test.rs"]
mod tests;
