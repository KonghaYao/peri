//! Micro Compact 实现
//!
//! 零 LLM 调用，对符合条件的旧消息标 `truncated`（不改内容）。
//! 计划生成由 planner 完成，本模块负责：
//! 1. 按 message_id 分组 action entries，构造 per-message `MessageProjectionDirective`
//! 2. 通过 `set_flags_projection` 持久化 directive（同时设置 truncated）
//!
//! 关键约束：
//! - planner 只能读取 transcript + config，绝不调用 set_truncated 等副作用 API
//! - per-call 粒度：同一 AI message 中不同 tool_call_id 独立决策

use std::collections::HashMap;
use tracing::debug;

use crate::agent::compact_v2::config::CompactConfig;
use crate::messages::MessageId;
use crate::session::transcript::MessageTranscript;

use super::projection::{MessageProjectionDirective, ProjectionActionEntry};

/// Micro Compact：调用 plan_micro 生成计划，然后持久化 per-message projection directive
///
/// 返回被标记的消息数量。
pub fn micro_compact(transcript: &mut MessageTranscript, config: &CompactConfig) -> usize {
    let plan = super::planner::plan_micro(transcript, config, true);

    // 按 message_id 分组 action entries，每个消息对应一个 MessageProjectionDirective
    let mut directives_by_msg: HashMap<MessageId, Vec<ProjectionActionEntry>> = HashMap::new();
    for action in &plan.actions {
        directives_by_msg
            .entry(action.message_id)
            .or_default()
            .push(action.clone());
    }

    let affected = directives_by_msg.len();

    // 对每组消息写入 projection directive（set_flags_projection 同时设置 truncated=true）
    for (msg_id, entries) in directives_by_msg {
        transcript.set_flags_projection(
            msg_id,
            MessageProjectionDirective {
                policy_version: plan.policy_version,
                entries,
            },
        );
    }

    if affected > 0 {
        debug!(
            affected,
            "Micro Compact: 持久化 projection directive + truncated 标记"
        );
    }

    affected
}

#[cfg(test)]
#[path = "micro_test.rs"]
mod tests;
