//! Receive 阶段 — 原子移交 MQ
//!
//! Receive 是循环入口和唯一消息消费点：存储在同一事务中把选中行写入
//! `messages` 并删除 MQ 行，成功后才 mirror Transcript、发布交付事件并进入
//! Compact/Reason/Act（`docs/design/thread-mq.md` §5.4）。

use crate::agent::events_v2::{ObserveEvent, RenderEvent, StateEvent};
use crate::agent::stages::StageContext;
use crate::session::{MessageKind, MessageSource, QueuedMessage, QueuedPayload};
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::session::{MessageDisposition, MqReceivePurpose, MqReceiveStatus, MqWatermark};
use peri_acp_types::store::PersistedPayload;

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
pub async fn run_receive(input: ReceiveInput) -> crate::error::AgentResult<ReceiveOutput> {
    run_receive_with_budget(input, None).await
}

pub(super) async fn run_receive_with_budget(
    input: ReceiveInput,
    exhausted_after: Option<MqWatermark>,
) -> crate::error::AgentResult<ReceiveOutput> {
    let execution = input.context.session.turn.execution_binding();
    let queue = input.context.session.queue.clone();
    // 同步 hook 的分离入队可能仍在提交：先等它落库，本轮选择才不会漏掉该消息。
    queue.await_pending_enqueues().await;
    let permit = queue.begin_receive(MqReceivePurpose::ModelLoop, execution.clone());
    let selected = queue
        .select_for_receive(&permit)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    // 停止意图只在产生它的执行中生效；resume 后新执行看到的旧 Stop 按
    // Suppressed 处理（存储只删行、不写历史，也不停新 run）。
    let stop_requested = selected.iter().any(|item| {
        item.entry.source == MessageSource::HookStopIntent
            && item.entry.policy.disposition(&execution) != MessageDisposition::Suppressed
    });
    // 预算耗尽时只消费循环开始时已存在的可消费前缀；其余留在 MQ，
    // 不能先出队再补历史。Stop 生效时不因预算推迟。
    let (consumed, deferred) = if stop_requested {
        (selected, Vec::new())
    } else if let Some(watermark) = exhausted_after.as_ref() {
        let cut = selected
            .iter()
            .position(|item| {
                item.entry.policy.ensures_processing() && item.entry.watermark() > *watermark
            })
            .unwrap_or(selected.len());
        let mut selected = selected;
        let deferred = selected.split_off(cut);
        (selected, deferred)
    } else {
        (selected, Vec::new())
    };
    let budget_deferred_count = deferred
        .iter()
        .filter(|item| item.entry.policy.ensures_processing())
        .count();
    // MQ 原子移交直接写存储；先排空 writer 中更早排队的历史追加，否则
    // 先 append 后移交（如 spawn 的身份 System）会在持久历史里倒序。
    if !consumed.is_empty() {
        let writer = input.context.session.transcript.read().persist_tx_handle();
        if let Some(writer) = writer {
            match crate::session::MessageTranscript::flush_via_tx(&writer).await {
                Ok(()) => {}
                // writer 已结束：后续追加同样会失败，交给 handoff 结构化上报。
                Err(error)
                    if error
                        .downcast_ref::<crate::session::transcript::FlushError>()
                        .is_some_and(|error| {
                            matches!(error, crate::session::transcript::FlushError::WriterClosed)
                        }) => {}
                Err(error) => return Err(anyhow::anyhow!(error).into()),
            }
        }
    }
    let outcome = queue
        .receive(permit, consumed)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;

    let mut user_inputs: Vec<(crate::messages::MessageId, crate::messages::MessageContent)> =
        Vec::new();
    let mut consumed_count = 0usize;
    let mut wake_up_count = 0usize;
    let mut conflict_count = 0usize;
    let mut prompt_count = 0usize;
    let mut defer_count = 0usize;
    let mut info_count = 0usize;
    let mut process_message_ids = Vec::new();
    for item in &outcome.items {
        let Some(entry) = &item.entry else {
            if item.status == MqReceiveStatus::Missing {
                tracing::warn!(
                    id = %item.id.as_uuid(),
                    "Receive: selected MQ row disappeared before handoff"
                );
            }
            continue;
        };
        match item.status {
            MqReceiveStatus::Suppressed => {
                consumed_count += 1;
                // 空 Prompt（keepgoing）只在存储里删行、不写历史，但仍须驱动本轮
                // 语义；旧执行绑定的 Stop 策略处置为 Suppressed，不得借此唤醒。
                if entry.policy.disposition(&execution) == MessageDisposition::Process {
                    wake_up_count += 1;
                }
                tracing::debug!(
                    id = %item.id.as_uuid(),
                    "Receive: suppressed stale MQ row without writing history"
                );
                continue;
            }
            MqReceiveStatus::Missing => continue,
            MqReceiveStatus::Conflict => {
                // 存储已删除冲突行，避免阻塞后续输入；历史保持不变。
                // 身份冲突必须显式失败（`docs/design/thread-mq.md` §7 故障处理），
                // 不能当作正常移交继续跑一轮模型调用。
                conflict_count += 1;
                tracing::warn!(
                    id = %item.id.as_uuid(),
                    "Receive: MQ row conflicts with existing history; row removed without write"
                );
                continue;
            }
            MqReceiveStatus::Committed | MqReceiveStatus::AlreadyPresent => {}
        }
        consumed_count += 1;
        match entry.kind {
            MessageKind::Prompt => prompt_count += 1,
            MessageKind::Defer => defer_count += 1,
            MessageKind::Info => info_count += 1,
        }
        if entry.policy.disposition(&execution) == MessageDisposition::Process {
            wake_up_count += 1;
            if let Some(id) = payload_id(&entry.payload) {
                process_message_ids.push(id);
            }
        }
        if entry.source == MessageSource::UserInput {
            if let PersistedPayload::Message(crate::messages::BaseMessage::Human { id, content }) =
                &entry.payload
            {
                if !content.is_empty() {
                    user_inputs.push((*id, content.clone()));
                }
            }
        }
        process_entry(&input.context, entry, item.status).await;
    }

    if consumed_count > 0 || budget_deferred_count > 0 {
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

    let user_ids: Vec<_> = user_inputs.iter().map(|(id, _)| *id).collect();
    if let Some(mailbox) = &input.context.session.user_input_mailbox {
        mailbox.mark_claimed(&user_ids).await;
    }
    if !user_inputs.is_empty() {
        if let Some(mailbox) = &input.context.session.user_input_mailbox {
            let delivered = mailbox.mark_delivered(&user_inputs).await;
            for (id, content) in &user_inputs {
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
                            content: content.clone(),
                        })
                        .await;
                }
            }
        }
    }

    tracing::debug!(
        turn_id = %input.context.session.turn.turn_id(),
        consumed_count,
        budget_deferred_count,
        "Receive 阶段原子移交 MQ"
    );

    if conflict_count > 0 {
        // 已处理的条目仍然有效；这里只把冲突事实显式暴露给上层，不静默继续。
        return Err(anyhow::anyhow!(
            "thread MQ identity conflict: {conflict_count} row(s) removed without writing history"
        )
        .into());
    }

    Ok(ReceiveOutput {
        consumed_count,
        wake_up_count,
        budget_deferred_count,
        input_message_ids: user_ids,
        process_message_ids,
        stop_requested,
    })
}

fn payload_id(payload: &PersistedPayload) -> Option<crate::messages::MessageId> {
    match payload {
        PersistedPayload::Message(message) => Some(message.id()),
        PersistedPayload::SystemReminder { id, .. } => Some(*id),
    }
}

/// 已提交行只 mirror 热态，不再触发同一条数据库写入。
async fn process_entry(
    context: &StageContext,
    entry: &peri_acp_types::session::MqEntry,
    status: MqReceiveStatus,
) {
    let message = QueuedMessage::from_mq_entry(entry.clone());
    let terminal_result = matches!(
        entry.source,
        MessageSource::ShellComplete
            | MessageSource::SubAgentComplete
            | MessageSource::WorkflowComplete
            | MessageSource::McpNotification
    );
    let delivery_id = message.delivery_id;
    let payload_id = payload_id(&entry.payload);

    if status == MqReceiveStatus::Committed {
        // 存储事务已经提交；这里只安装热态，禁止再走异步持久化 writer。
        let newly_committed = context
            .session
            .transcript
            .write()
            .mirror_committed_payload(entry.payload.clone());
        if let (Some(delivery_id), true) = (delivery_id, terminal_result) {
            if let PersistedPayload::SystemReminder { reminder, .. } = &entry.payload {
                if let Err(error) = context.session.async_result_mailbox.mark_canonical_ready(
                    delivery_id,
                    reminder,
                    entry.source.clone(),
                    None,
                ) {
                    // 身份冲突是确定性的：结果保护责任无法登记，但 MQ 行已移交，
                    // 不能回滚；留下可关联诊断并继续处理后续输入。
                    tracing::warn!(
                        delivery_id = %delivery_id.as_uuid(),
                        %error,
                        "conflicting canonical terminal delivery identity"
                    );
                }
            }
        }
        if !newly_committed {
            return;
        }
        emit_entry(context, &message, &entry.payload).await;
        return;
    }

    // AlreadyPresent：历史已有同 ID 同内容，不再发射；热态缺失且未排除时补 mirror。
    let hot_missing =
        payload_id.is_some_and(|id| context.session.transcript.read().get(id).is_none());
    if !hot_missing {
        return;
    }
    if let PersistedPayload::SystemReminder { reminder, .. } = &entry.payload {
        let port = context.session.transcript.read().idempotent_reminder_port();
        let Some((resources, thread_id, _)) = port else {
            context
                .session
                .transcript
                .write()
                .mirror_committed_reminder(delivery_id.unwrap_or_default(), reminder.clone());
            return;
        };
        let id = delivery_id.unwrap_or_default();
        match resources.load_message_flags(&thread_id, &[id]).await {
            Ok(flags) if flags.get(&id).is_some_and(|flags| flags.excluded) => {
                // 归档原文不得回灌热态；派生模型视图会按保护记录重新投影。
                tracing::warn!(
                    delivery_id = %id.as_uuid(),
                    "protected async result canonical reminder is excluded; \
                     derived model view will re-append it"
                );
            }
            Ok(_) => {
                context
                    .session
                    .transcript
                    .write()
                    .mirror_committed_reminder(id, reminder.clone());
            }
            Err(error) => {
                tracing::warn!(
                    delivery_id = %id.as_uuid(),
                    %error,
                    "already-present reminder flags unavailable; hot transcript not mirrored"
                );
            }
        }
    } else {
        context
            .session
            .transcript
            .write()
            .mirror_committed_payload(entry.payload.clone());
    }
}

async fn emit_entry(context: &StageContext, message: &QueuedMessage, payload: &PersistedPayload) {
    if let PersistedPayload::SystemReminder { reminder, .. } = payload {
        let outcome = context
            .runtime
            .event_bus
            .emit_state_reliable(StateEvent::ProtocolEvent {
                turn_id: context.turn_id(),
                agent_id: context.session.agent_id,
                event: Box::new(ExecutorEvent::SystemReminder {
                    reminder: reminder.as_reminder().clone(),
                    source_agent_id: None,
                }),
            })
            .await;
        if outcome == peri_acp_types::event_v2::EmitOutcome::Closed {
            tracing::warn!(
                turn_id = %context.turn_id(),
                "ProtocolEvent 接收端关闭；总线已记录需重同步"
            );
        }
    } else if let Some(text) = synthetic_defer_text(message) {
        context
            .runtime
            .event_bus
            .emit_state(StateEvent::SyntheticUserMessage {
                turn_id: context.turn_id(),
                agent_id: context.session.agent_id,
                text,
            });
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "receive_test.rs"]
mod tests;

#[cfg(test)]
#[path = "receive_delivery_test.rs"]
mod delivery_tests;
