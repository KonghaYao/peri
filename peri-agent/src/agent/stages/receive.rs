//! Receive 阶段 — 排空收件箱
//!
//! 从 MessageQueue 中取出 Prompt + Info 消息，写入 Transcript。
//! Defer 消息保留在队列中，等待 End 阶段或下个 turn。

use crate::agent::stages::{ReceiveInput, ReceiveOutput, append_messages_to_transcript};
#[cfg(test)]
use crate::session::QueuedMessage;

/// 运行 Receive 阶段
///
/// 调用 `drain_for_receive()` 消费 Prompt + Info，将消息内容写入 Transcript
/// （通过共享 helper `append_messages_to_transcript`，与 End 阶段的 Defer 写入
/// 保持一致的包裹语义）。
pub async fn run_receive(input: ReceiveInput) -> crate::error::AgentResult<ReceiveOutput> {
    let consumed = input.context.queue.drain_for_receive();
    let count = consumed.len();

    if count > 0 {
        let mut transcript = input.context.transcript.write();
        append_messages_to_transcript(&mut transcript, consumed);
        tracing::debug!(
            turn_id = %input.context.turn.turn_id,
            count,
            "Receive 阶段消费消息"
        );
    }

    Ok(ReceiveOutput {
        consumed_count: count,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::stages::StageContext;
    use crate::messages::{BaseMessage, MessageContent};
    use crate::session::Session;
    use crate::session::queue::MessageSource;
    use crate::session::store::FrozenContext;
    use std::sync::Arc;

    fn make_context() -> StageContext {
        let cwd: Arc<str> = Arc::from("/tmp/test");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);
        let turn = session.start_turn();
        StageContext::new(turn, session.transcript(), session.queue().clone())
    }

    #[tokio::test]
    async fn test_receive_empty_queue() {
        let ctx = make_context();
        let input = ReceiveInput {
            context: ctx.clone(),
        };
        let output = run_receive(input).await.unwrap();
        assert_eq!(output.consumed_count, 0);
        assert!(ctx.transcript.read().is_empty());
    }

    #[tokio::test]
    async fn test_receive_consumes_prompt() {
        let ctx = make_context();
        ctx.queue.push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(MessageContent::text("hello")),
        ));

        let input = ReceiveInput {
            context: ctx.clone(),
        };
        let output = run_receive(input).await.unwrap();
        assert_eq!(output.consumed_count, 1);
        assert_eq!(ctx.transcript.read().len(), 1);
    }

    #[tokio::test]
    async fn test_receive_consumes_info_wrapped_in_reminder() {
        let ctx = make_context();
        ctx.queue.push(QueuedMessage::info(
            MessageSource::SystemInjected,
            BaseMessage::human(MessageContent::text("system info")),
        ));

        let input = ReceiveInput {
            context: ctx.clone(),
        };
        let output = run_receive(input).await.unwrap();
        assert_eq!(output.consumed_count, 1);

        let transcript = ctx.transcript.read();
        assert_eq!(transcript.len(), 1);
        let content = transcript.entries()[0].message.content();
        assert!(
            content.contains("<system-reminder>"),
            "Info 应被 reminder 包裹"
        );
        assert!(content.contains("system info"));
    }

    #[tokio::test]
    async fn test_receive_keeps_defer() {
        let ctx = make_context();
        ctx.queue.push(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human(MessageContent::text("deferred")),
        ));
        ctx.queue.push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(MessageContent::text("prompt")),
        ));

        let input = ReceiveInput {
            context: ctx.clone(),
        };
        let output = run_receive(input).await.unwrap();
        // 只消费 Prompt，Defer 保留
        assert_eq!(output.consumed_count, 1);
        assert_eq!(ctx.queue.len(), 1, "Defer 应保留在队列");
        assert_eq!(ctx.transcript.read().len(), 1);
    }
}
