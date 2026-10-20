//! Receive 阶段 — 排空收件箱
//!
//! 从 MessageQueue 中取出所有消息（Prompt + Info + Defer），写入 Transcript。
//! RCRA 重构后，Receive 是循环入口和唯一消息消费点，不再有 End 阶段单独消费 Defer。

use crate::agent::events_v2::{ObserveEvent, RenderEvent, StateEvent};
use crate::agent::stages::append_messages_to_transcript;
use crate::agent::stages::StageContext;
use crate::session::{MessageKind, MessageSource, QueuedMessage, QueuedPayload};
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::session::MessageDisposition;

// ─── Receive 阶段类型 ────────────────────────────────────────────────────────

/// Receive 阶段输入
pub struct ReceiveInput {
    pub context: StageContext,
}

/// Receive 阶段输出
pub struct ReceiveOutput {
    /// 本轮消费的消息数量（含不唤醒循环的 Info）
    pub consumed_count: usize,
    /// 本轮消费的可驱动语义续跑消息数量（Prompt / Defer）
    pub wake_up_count: usize,
    /// 预算不足而保留在队列中的后续新增 EnsureProcessing 输入数量。
    pub budget_deferred_count: usize,
    /// 本次消费的非空用户 Human 消息身份，供首次输入准备精准处理。
    pub input_message_ids: Vec<crate::messages::MessageId>,
    /// 本轮已 canonical 提交、且本应驱动语义迭代的消息身份；由 loop 在
    /// 判定收尾截止时统一登记为未推理义务，避免 Receive 内部 check-then-check 竞态。
    pub process_message_ids: Vec<crate::messages::MessageId>,
    /// 本轮消费到 hook 的显式停止意图（`continue:false`）：循环必须经 Receive
    /// 唯一退出口结束，不得再发起额外模型请求。
    pub stop_requested: bool,
}

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
    run_receive_with_budget(input, None).await
}

pub(super) async fn run_receive_with_budget(
    input: ReceiveInput,
    exhausted_after: Option<u64>,
) -> crate::error::AgentResult<ReceiveOutput> {
    let execution = input.context.session.turn.execution_binding();
    let consumed = input.context.session.queue.drain_batch(64);
    // 停止意图只在产生它的执行中生效；resume 后新执行看到的旧 Stop 按
    // Suppressed 处理（下方统一移入 suppressed，不写 transcript，也不停新 run）。
    let stop_requested = consumed.iter().any(|message| {
        message.source == MessageSource::HookStopIntent
            && message.policy.disposition(&execution) != MessageDisposition::Suppressed
    });
    let (deferred, consumed): (Vec<_>, Vec<_>) = consumed.into_iter().partition(|message| {
        !stop_requested
            && exhausted_after.is_some_and(|watermark| {
                message.policy.ensures_processing()
                    && message
                        .admission_sequence
                        .is_some_and(|sequence| sequence > watermark)
            })
    });
    let budget_deferred_count = deferred.len();
    input.context.session.queue.push_batch(deferred);
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
    let wake_up_count = consumed
        .iter()
        .filter(|message| message.policy.disposition(&execution) == MessageDisposition::Process)
        .count();
    let process_message_ids: Vec<_> = consumed
        .iter()
        .filter(|message| message.policy.disposition(&execution) == MessageDisposition::Process)
        .filter_map(|message| {
            message
                .delivery_id
                .or_else(|| message.message().map(crate::messages::BaseMessage::id))
        })
        .collect();

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
            if msg.policy.disposition(&execution) == MessageDisposition::Suppressed {
                input.context.session.queue.suppress(msg.clone());
                continue;
            }
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
                            input
                                .context
                                .session
                                .queue
                                .push_batch(consumed[index..].to_vec());
                            return Err(anyhow::anyhow!(
                                "conflicting canonical terminal delivery ID"
                            )
                            .into());
                        }
                    };
                    (present, transcript.idempotent_reminder_port())
                };
                if already_present {
                    // 已安装（同一 ID 同内容）：不重复发射，也不重复安装。
                    newly_committed = false;
                } else if let Some((resources, thread_id, writer)) = port {
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
                    match committed {
                        // (a) 本次提交成功：安装并作为新交付发射。
                        Ok(true) => {
                            newly_committed = input
                                .context
                                .session
                                .transcript
                                .write()
                                .mirror_committed_reminder(delivery_id, reminder.clone());
                        }
                        // 持久化事实已存在但热态没有（如 Full Compact 收缩释放了它）：
                        // (b) 仍有效 → 补安装，不作为新交付发射；(c) 已排除 → 两者都不做。
                        Ok(false) => {
                            newly_committed = false;
                            let flags = resources
                                .load_message_flags(&thread_id, &[delivery_id])
                                .await;
                            let flags = match flags {
                                Ok(flags) => flags,
                                Err(error) => {
                                    input
                                        .context
                                        .session
                                        .queue
                                        .push_batch(consumed[index..].to_vec());
                                    return Err(anyhow::Error::from(error).into());
                                }
                            };
                            // 该 ID 刚被证明已提交，缺席只表示后端没有记录排除事实；
                            // 只有读到的 excluded 才阻止补安装（归档原文不得回灌热态）。
                            if !flags.get(&delivery_id).is_some_and(|flags| flags.excluded) {
                                input
                                    .context
                                    .session
                                    .transcript
                                    .write()
                                    .mirror_committed_reminder(delivery_id, reminder.clone());
                            } else {
                                // 保护约束被破坏：该结果仍在 mailbox 中，派生模型视图
                                // 会按记录重新投影；这里留下可关联诊断，不回灌全部历史。
                                tracing::warn!(
                                    delivery_id = %delivery_id.as_uuid(),
                                    "protected async result canonical reminder is excluded; \
                                     derived model view will re-append it"
                                );
                            }
                        }
                        Err(error) => {
                            input
                                .context
                                .session
                                .queue
                                .push_batch(consumed[index..].to_vec());
                            return Err(error.into());
                        }
                    }
                } else {
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
            // 只有正式终态结果才建立处理责任；cron 等带 delivery_id 的普通输入
            // 仍由队列与既有激活规则负责，不能混入结果保护集合。
            // 只有规范写入成功（或匹配到同一待处理记录）后才进入 Ready。
            let terminal_result = matches!(
                msg.source,
                MessageSource::ShellComplete
                    | MessageSource::SubAgentComplete
                    | MessageSource::WorkflowComplete
                    | MessageSource::DynamicMcpNotification
            );
            if let Some(delivery_id) = msg.delivery_id.filter(|_| terminal_result) {
                if let QueuedPayload::SystemReminder(reminder) = &msg.payload {
                    if let Err(error) = input
                        .context
                        .session
                        .async_result_mailbox
                        .mark_canonical_ready(
                            delivery_id,
                            reminder,
                            msg.source.clone(),
                            msg.admission_sequence,
                        )
                    {
                        input
                            .context
                            .session
                            .queue
                            .push_batch(consumed[index..].to_vec());
                        return Err(anyhow::anyhow!(error).into());
                    }
                }
            }
            if !newly_committed {
                continue;
            }
            if let QueuedPayload::SystemReminder(reminder) = &msg.payload {
                let outcome = input
                    .context
                    .runtime
                    .event_bus
                    .emit_state_reliable(StateEvent::ProtocolEvent {
                        turn_id: input.context.turn_id(),
                        agent_id: input.context.session.agent_id,
                        event: Box::new(ExecutorEvent::SystemReminder {
                            reminder: reminder.as_reminder().clone(),
                            source_agent_id: None,
                        }),
                    })
                    .await;
                if outcome == peri_acp_types::event_v2::EmitOutcome::Closed {
                    tracing::warn!(turn_id = %input.context.turn_id(), "ProtocolEvent 接收端关闭；总线已记录需重同步");
                }
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
                    // 交付事件走同一条 render FIFO，但只等待容量、不降级丢弃：
                    // forwarder 按 FIFO 消费时补打 session_seq 并绑定交付日志，
                    // 事件在后续任意路径丢失时仍能按该序号在水位前补发。
                    input
                        .context
                        .runtime
                        .event_bus
                        .emit_render_reliable(RenderEvent::UserInputDelivered {
                            turn_id: input.context.turn_id(),
                            agent_id: input.context.session.agent_id,
                            generation: mailbox.generation().to_owned(),
                            input_id,
                            content,
                        })
                        .await;
                }
            }
        }
        tracing::debug!(
            turn_id = %input.context.session.turn.turn_id(),
            count,
            "Receive 阶段消费消息"
        );
    }

    Ok(ReceiveOutput {
        consumed_count: count,
        wake_up_count,
        budget_deferred_count,
        input_message_ids: user_ids,
        process_message_ids,
        // hook 的显式停止意图：本轮消费到就必须经 Receive 唯一退出口结束。
        stop_requested,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "receive_test.rs"]
mod tests;
