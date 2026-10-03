//! Receive 阶段 — 排空收件箱
//!
//! 从 MessageQueue 中取出所有消息（Prompt + Info + Defer），写入 Transcript。
//! RCRA 重构后，Receive 是循环入口和唯一消息消费点，不再有 End 阶段单独消费 Defer。

use crate::agent::events_v2::{ObserveEvent, RenderEvent, StateEvent};
use crate::agent::stages::{append_messages_to_transcript, ReceiveInput, ReceiveOutput};
use crate::session::{MessageKind, MessageSource, QueuedMessage, QueuedPayload};
use peri_acp_types::event::ExecutorEvent;

fn synthetic_defer_text(message: &QueuedMessage) -> Option<String> {
    match (&message.kind, &message.payload) {
        (MessageKind::Defer, QueuedPayload::Message(message)) => {
            Some(message.content().to_string())
        }
        _ => None,
    }
}

/// 运行 Receive 阶段
///
/// 调用 `drain_all()` 消费队列中全部消息（Prompt + Info + Defer）。
/// 对 Defer 消息 emit `SyntheticUserMessage` 事件（TUI bridge 刷新 committed 视图用）。
/// 消费后通过共享 helper `append_messages_to_transcript` 写入 Transcript。
pub async fn run_receive(input: ReceiveInput) -> crate::error::AgentResult<ReceiveOutput> {
    let consumed = input.context.session.queue.drain_all();
    let user_inputs: Vec<_> = consumed
        .iter()
        .filter(|message| message.source == MessageSource::UserInput)
        .filter_map(|message| match message.message() {
            Some(crate::messages::BaseMessage::Human { id, content }) if !content.is_empty() => {
                Some((*id, content.clone()))
            }
            _ => None,
        })
        .collect();
    let user_ids: Vec<_> = user_inputs.iter().map(|(id, _)| *id).collect();
    if let Some(mailbox) = &input.context.session.user_input_mailbox {
        mailbox.mark_claimed(&user_ids);
    }
    let count = consumed.len();
    let mut wake_up_count = consumed
        .iter()
        .filter(|message| message.kind.wakes_up())
        .count();

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
        for (index, msg) in consumed.iter().enumerate() {
            let mut newly_committed = true;
            if let Some(delivery_id) = msg.delivery_id {
                let QueuedPayload::SystemReminder(reminder) = &msg.payload else {
                    input
                        .context
                        .session
                        .queue
                        .push_batch(consumed[index..].to_vec());
                    return Err(anyhow::anyhow!("delivery ID requires a canonical reminder").into());
                };
                let (already_present, port) = {
                    let transcript = input.context.session.transcript.read();
                    let present = match transcript.get(delivery_id) {
                        None => false,
                        Some(crate::session::TranscriptEntry::Reminder {
                            reminder: stored,
                            ..
                        }) if stored.as_reminder() == reminder.as_reminder() => true,
                        Some(_) => {
                            return Err(anyhow::anyhow!(
                                "conflicting canonical terminal delivery ID"
                            )
                            .into());
                        }
                    };
                    (present, transcript.idempotent_reminder_port())
                };
                if already_present {
                    newly_committed = false;
                } else {
                    if let Some((resources, thread_id, writer)) = port {
                        let committed = async {
                            if let Some(writer) = writer {
                                crate::session::MessageTranscript::flush_via_tx(&writer).await?;
                            }
                            resources
                                .append_reminder_if_absent(&thread_id, delivery_id, reminder)
                                .await
                                .map_err(anyhow::Error::from)
                        }
                        .await;
                        if let Err(error) = committed {
                            input
                                .context
                                .session
                                .queue
                                .push_batch(consumed[index..].to_vec());
                            return Err(error.into());
                        }
                    }
                    newly_committed = input
                        .context
                        .session
                        .transcript
                        .write()
                        .mirror_committed_reminder(delivery_id, reminder.clone());
                }
            } else {
                let mut transcript = input.context.session.transcript.write();
                append_messages_to_transcript(&mut transcript, vec![msg.clone()]);
            }
            if !newly_committed {
                if msg.kind.wakes_up() {
                    wake_up_count -= 1;
                }
                continue;
            }
            if let QueuedPayload::SystemReminder(reminder) = &msg.payload {
                input
                    .context
                    .runtime
                    .event_bus
                    .emit_state(StateEvent::ProtocolEvent {
                        turn_id: input.context.turn_id(),
                        agent_id: input.context.session.agent_id,
                        event: ExecutorEvent::SystemReminder(reminder.as_reminder().clone()),
                    });
            } else if let Some(text) = synthetic_defer_text(msg) {
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
        if let Some(mailbox) = &input.context.session.user_input_mailbox {
            let delivered = mailbox.mark_delivered(&user_ids);
            for (id, content) in user_inputs {
                let input_id = id.as_uuid().to_string();
                if delivered.contains(&input_id) {
                    input
                        .context
                        .runtime
                        .event_bus
                        .emit_render(RenderEvent::UserInputDelivered {
                            turn_id: input.context.turn_id(),
                            agent_id: input.context.session.agent_id,
                            generation: mailbox.generation().to_owned(),
                            input_id,
                            content,
                        });
                }
            }
        }
        tracing::debug!(
            turn_id = %input.context.session.turn.turn_id,
            count,
            "Receive 阶段消费消息"
        );
    }

    Ok(ReceiveOutput {
        consumed_count: count,
        wake_up_count,
        input_message_ids: user_ids,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "receive_test.rs"]
mod tests;
