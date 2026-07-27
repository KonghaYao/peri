//! Receive 阶段 — 排空收件箱
//!
//! 从 MessageQueue 中取出所有消息（Prompt + Info + Defer），写入 Transcript。
//! RCRA 重构后，Receive 是循环入口和唯一消息消费点，不再有 End 阶段单独消费 Defer。

use crate::agent::events_v2::{ObserveEvent, StateEvent};
use crate::agent::stages::{append_messages_to_transcript, ReceiveInput, ReceiveOutput};
use crate::session::MessageKind;

/// 运行 Receive 阶段
///
/// 调用 `drain_all()` 消费队列中全部消息（Prompt + Info + Defer）。
/// 对 Defer 消息 emit `SyntheticUserMessage` 事件（TUI bridge 刷新 committed 视图用）。
/// 消费后通过共享 helper `append_messages_to_transcript` 写入 Transcript。
pub async fn run_receive(input: ReceiveInput) -> crate::error::AgentResult<ReceiveOutput> {
    let consumed = input.context.session.queue.drain_all();
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
        // 在写入 transcript 前，对 Defer 消息 emit SyntheticUserMessage
        // （复制原 End 阶段 post-wake drain 的同模式 emit，让 TUI bridge 刷新 committed 视图）
        for msg in &consumed {
            if msg.kind == MessageKind::Defer {
                let raw_text = msg.message.content().to_string();
                let text = format!("<system-reminder>\n{}\n</system-reminder>", raw_text);
                input
                    .context
                    .runtime
                    .event_bus
                    .emit_state(StateEvent::SyntheticUserMessage {
                        turn_id: input.context.turn_id(),
                        agent_id: input.context.session.agent_id,
                        text,
                    });
            }
        }

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
