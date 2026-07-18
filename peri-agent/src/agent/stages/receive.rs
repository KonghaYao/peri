//! Receive 阶段 — 排空收件箱
//!
//! 从 MessageQueue 中取出 Prompt + Info 消息，写入 Transcript。
//! Defer 消息保留在队列中，等待 End 阶段或下个 turn。

use crate::agent::events_v2::ObserveEvent;
use crate::agent::stages::{append_messages_to_transcript, ReceiveInput, ReceiveOutput};
use crate::session::MessageKind;

/// 运行 Receive 阶段
///
/// 调用 `drain_for_receive()` 消费 Prompt + Info，将消息内容写入 Transcript
/// （通过共享 helper `append_messages_to_transcript`，与 End 阶段的 Defer 写入
/// 保持一致的包裹语义）。
pub async fn run_receive(input: ReceiveInput) -> crate::error::AgentResult<ReceiveOutput> {
    let consumed = input.context.session.queue.drain_for_receive();
    let count = consumed.len();

    // emit MessageQueueDrained（langfuse v2 遥测）
    {
        let mut prompt_count = 0usize;
        let mut defer_count = 0usize;
        let mut info_count = 0usize;
        for msg in &consumed {
            match msg.kind {
                MessageKind::Prompt => prompt_count += 1,
                MessageKind::Defer => defer_count += 1,
                MessageKind::Info => info_count += 1,
            }
        }
        input
            .context
            .runtime
            .event_bus
            .emit_observe(ObserveEvent::MessageQueueDrained {
                turn_id: input.context.turn_id(),
                agent_id: input.context.session.agent_id,
                prompt: prompt_count,
                defer: defer_count,
                info: info_count,
            });
    }

    if count > 0 {
        let mut transcript = input.context.session.transcript.write();
        append_messages_to_transcript(&mut transcript, consumed);
        tracing::debug!(
            turn_id = %input.context.session.turn.turn_id,
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
#[path = "receive_test.rs"]
mod tests;
