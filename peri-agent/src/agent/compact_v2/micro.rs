//! Micro Compact 实现
//!
//! 零 LLM 调用，对符合条件的旧消息标 `truncated`（不改内容）。
//! 现已收缩为 `plan_micro` 的包装——计划生成由 planner 完成，
//! 本模块仅负责应用 truncated 标记。
//!
//! 关键约束：
//! - planner 只能读取 transcript + config，绝不调用 set_truncated 等副作用 API
//! - per-call 粒度：同一 AI message 中不同 tool_call_id 独立决策

use tracing::debug;

use crate::agent::compact_v2::config::CompactConfig;
use crate::session::transcript::MessageTranscript;

use super::projection::ProjectionTarget;

/// Micro Compact：调用 plan_micro 生成计划，然后应用 truncated 标记
///
/// 返回被标记的消息数量。
pub fn micro_compact(transcript: &mut MessageTranscript, config: &CompactConfig) -> usize {
    let plan = super::planner::plan_micro(transcript, config, true);
    let affected = plan.actions.len();

    for action in &plan.actions {
        match &action.target {
            ProjectionTarget::Message
            | ProjectionTarget::ContentBlock { .. }
            | ProjectionTarget::ToolCall { .. } => {
                transcript.set_truncated(action.message_id, true);
            }
        }
    }

    if affected > 0 {
        debug!(affected, "Micro Compact: 标记 truncated 消息");
    }

    affected
}

#[cfg(test)]
#[path = "micro_test.rs"]
mod tests;
