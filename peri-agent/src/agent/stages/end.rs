//! End 阶段 — 交还控制权
//!
//! 检查 MessageQueue 是否有 Prompt/Defer 消息：
//! - 有 → 返回 should_continue = true，排空唤醒消息
//! - 无（队列空或仅有 Info）→ 返回 should_continue = false，循环退出

use crate::agent::stages::{EndInput, EndOutput};

/// 运行 End 阶段
///
/// 调用 `drain_for_end()` 检查队列唤醒条件。
pub fn run_end(input: EndInput) -> EndOutput {
    let result = input.context.session.queue.drain_for_end();

    match result {
        Some(messages) => {
            let count = messages.len();
            tracing::debug!(
                turn_id = %input.context.session.turn.turn_id,
                awakened = count,
                "End 阶段：有新消息，激活新 turn"
            );
            EndOutput {
                should_continue: true,
                awakened_messages: messages,
            }
        }
        None => {
            tracing::debug!(
                turn_id = %input.context.session.turn.turn_id,
                "End 阶段：队列为空或仅有 Info，退出循环"
            );
            EndOutput {
                should_continue: false,
                awakened_messages: vec![],
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "end_test.rs"]
mod tests;
