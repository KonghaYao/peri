//! End 阶段 — 交还控制权
//!
//! 检查 MessageQueue 是否有 Prompt/Defer 消息：
//! - 有 → 返回 should_continue = true，排空唤醒消息
//! - 无（队列空或仅有 Info）→ 返回 should_continue = false，循环退出

use crate::agent::stages::{EndInput, EndOutput};
#[cfg(test)]
use crate::session::QueuedMessage;

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
mod tests {
    use super::*;
    use crate::agent::stages::StageContext;
    use crate::messages::{BaseMessage, MessageContent};
    use crate::session::queue::MessageSource;
    use crate::session::store::FrozenContext;
    use crate::session::Session;
    use std::sync::Arc;

    fn make_context() -> StageContext {
        let cwd: Arc<str> = Arc::from("/tmp/test");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        StageContext::new(turn, session.transcript(), session.queue().clone())
    }

    #[test]
    fn test_end_empty_queue_stops() {
        let ctx = make_context();
        let input = EndInput { context: ctx };
        let output = run_end(input);
        assert!(!output.should_continue);
        assert!(output.awakened_messages.is_empty());
    }

    #[test]
    fn test_end_prompt_wakes() {
        let ctx = make_context();
        ctx.session.queue.push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(MessageContent::text("new question")),
        ));
        let input = EndInput { context: ctx };
        let output = run_end(input);
        assert!(output.should_continue);
        assert_eq!(output.awakened_messages.len(), 1);
    }

    #[test]
    fn test_end_defer_wakes() {
        let ctx = make_context();
        ctx.session.queue.push(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human(MessageContent::text("deferred")),
        ));
        let input = EndInput { context: ctx };
        let output = run_end(input);
        assert!(output.should_continue);
    }

    #[test]
    fn test_end_info_only_does_not_wake() {
        let ctx = make_context();
        ctx.session.queue.push(QueuedMessage::info(
            MessageSource::SystemInjected,
            BaseMessage::human(MessageContent::text("info")),
        ));
        let input = EndInput {
            context: ctx.clone(),
        };
        let output = run_end(input);
        assert!(!output.should_continue, "仅有 Info 不应唤醒");
        // Info 保留在队列
        assert_eq!(ctx.session.queue.len(), 1);
    }

    #[test]
    fn test_end_prompt_plus_info_wakes_only_prompt() {
        let ctx = make_context();
        ctx.session.queue.push(QueuedMessage::info(
            MessageSource::SystemInjected,
            BaseMessage::human(MessageContent::text("info")),
        ));
        ctx.session.queue.push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(MessageContent::text("prompt")),
        ));
        let input = EndInput {
            context: ctx.clone(),
        };
        let output = run_end(input);
        assert!(output.should_continue);
        // 只消费 Prompt，Info 保留
        assert_eq!(ctx.session.queue.len(), 1, "Info 应保留在队列");
    }
}
