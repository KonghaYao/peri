//! Micro Compact 实现
//!
//! 零 LLM 调用，对符合条件的旧消息标 `truncated`（不改内容）。
//! 策略：
//! - 仅操作自有消息（ancestor_len 之后）
//! - 按 round 分组，跳过最近 `micro_compact_stale_steps` 轮
//! - 对白名单工具的 Tool 消息标 truncated
//! - 对含 Image/Document 的消息标 truncated

use tracing::debug;

use crate::agent::compact_v2::config::CompactConfig;
use crate::messages::{BaseMessage, ContentBlock};
use crate::session::transcript::{MessageTranscript, TranscriptEntry};

/// Micro Compact：零 LLM 调用，对符合条件的旧消息标 `truncated`
///
/// 策略：
/// - 仅操作自有消息（ancestor_len 之后）
/// - 按 round 分组，跳过最近 `micro_compact_stale_steps` 轮
/// - 对白名单工具的 Tool 消息标 truncated
/// - 对含 Image/Document 的消息标 truncated
///
/// 返回被标记的消息数量。
pub fn micro_compact(transcript: &mut MessageTranscript, config: &CompactConfig) -> usize {
    /// 检查工具名是否应被截断（不在黑名单中）
    fn tool_should_truncate(tool_name: &str, excluded: &[String]) -> bool {
        !excluded.iter().any(|e| e.eq_ignore_ascii_case(tool_name))
    }

    let ancestor_len = transcript.ancestor_len();
    let entries = transcript.entries();
    if entries.len() <= ancestor_len {
        return 0;
    }

    // 按 round 分组自有消息（基于条目索引）
    let own_start = ancestor_len;
    let own_entries = &entries[own_start..];
    let round_starts = compute_round_starts(own_entries);
    let total_rounds = round_starts.len();
    let stale_limit = total_rounds.saturating_sub(config.micro_compact_stale_steps);

    // 构建消息 → round 索引
    let mut round_index = vec![0usize; own_entries.len()];
    for (ri, &start) in round_starts.iter().enumerate() {
        let end = if ri + 1 < round_starts.len() {
            round_starts[ri + 1]
        } else {
            own_entries.len()
        };
        let last = end.min(own_entries.len());
        if start < last {
            for slot in &mut round_index[start..last] {
                *slot = ri;
            }
        }
    }

    // 先收集所有待标记的 id（避免借用冲突）
    let mut ids_to_truncate: Vec<_> = Vec::new();
    for (i, entry) in own_entries.iter().enumerate() {
        // 跳过最近 N 轮
        if round_index[i] >= stale_limit {
            continue;
        }

        let msg = &entry.message;
        let id = msg.id();

        // 检查是否已被 truncated（避免重复标记）
        let existing_flags = transcript.flags(id);
        if existing_flags.truncated {
            continue;
        }

        let should_truncate = match msg {
            // Tool 消息（tool_result 输出）→ 工具名不在黑名单则截断
            BaseMessage::Tool {
                tool_call_id,
                is_error,
                ..
            } => {
                if *is_error {
                    false // 错误输出不截断，保留诊断信息
                } else {
                    let tool_name = find_tool_name_for_result(own_entries, i, tool_call_id);
                    tool_name
                        .map(|n| tool_should_truncate(&n, &config.micro_excluded_tools))
                        .unwrap_or(false)
                }
            }
            // Ai 消息（tool_use 输入 arguments）→ 任一 tool_call 不在黑名单则截断
            BaseMessage::Ai { tool_calls, .. } => tool_calls
                .iter()
                .any(|tc| tool_should_truncate(&tc.name, &config.micro_excluded_tools)),
            // 其他消息：检查 Image/Document 块
            _ => {
                let blocks = msg.message_content().content_blocks();
                blocks.iter().any(|b| {
                    matches!(
                        b,
                        ContentBlock::Image { .. } | ContentBlock::Document { .. }
                    )
                })
            }
        };

        if should_truncate {
            ids_to_truncate.push(id);
        }
    }

    // 批量设置 truncated 标记
    for id in &ids_to_truncate {
        transcript.set_truncated(*id, true);
    }

    let affected = ids_to_truncate.len();
    if affected > 0 {
        debug!(affected, "Micro Compact: 标记 truncated 消息");
    }

    affected
}

/// 在 0..pos 范围内向前查找 tool_call_id 对应的工具名称。
fn find_tool_name_for_result(
    entries: &[TranscriptEntry],
    result_pos: usize,
    tool_call_id: &str,
) -> Option<String> {
    // 就近查找：Ai 消息通常在 tool_result 之前 1-2 条
    let _start = result_pos.saturating_sub(5);
    for entry in entries[..result_pos].iter().rev().take(5) {
        if let BaseMessage::Ai { tool_calls, .. } = &entry.message {
            for tc in tool_calls {
                if tc.id == tool_call_id {
                    return Some(tc.name.clone());
                }
            }
        }
    }
    // 全范围回退（兼容异常排序）
    for entry in entries[..result_pos].iter().rev() {
        if let BaseMessage::Ai { tool_calls, .. } = &entry.message {
            for tc in tool_calls {
                if tc.id == tool_call_id {
                    return Some(tc.name.clone());
                }
            }
        }
    }
    None
}

/// 按 round 边界计算每个 round 的起始索引
///
/// 简化版分组：每条消息自成一个 round，但 AI+Tool 组合视为一个 round。
/// 返回每个 round 的起始索引列表。
fn compute_round_starts(entries: &[TranscriptEntry]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        starts.push(i);
        if let BaseMessage::Ai { tool_calls, .. } = &entries[i].message {
            if !tool_calls.is_empty() {
                let tc_count = tool_calls.len();
                let mut end = i + 1;
                let mut matched = 0;
                while end < entries.len() && matched < tc_count {
                    if let BaseMessage::Tool { tool_call_id, .. } = &entries[end].message {
                        if tool_calls.iter().any(|tc| tc.id == *tool_call_id) {
                            matched += 1;
                            end += 1;
                            continue;
                        }
                    }
                    break;
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    starts
}

#[cfg(test)]
#[path = "micro_test.rs"]
mod tests;
