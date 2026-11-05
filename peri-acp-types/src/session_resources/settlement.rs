//! 跨行为原子操作的**回退组合**。
//!
//! 本机 SQLite 后端用单个事务覆写这两条行为；没有跨行为事务的后端（远端存储、
//! 测试替身）只能按既有单行为逐条组合。差异只在崩溃窗口：组合实现保证「失败前
//! 不宣称完成」，但进程在两步之间退出仍可能留下已写的终态或已写的提醒。
//! 调用方因此不得把这里的成功当成事务提交的证据，也不得依赖它做补偿。

use async_trait::async_trait;

use crate::messages::MessageId;
use crate::session::{
    ExecutionBinding, MqEnqueueDisposition, MqEnqueueOutcome, MqEntry, MqInput,
    MqReceiveDisposition, MqReceiveItem, MqReceiveOutcome, MqReceivePurpose, MqReceiveSelection,
    RetainedReminder, TurnId,
};
use crate::session_resources::{SessionMetaPatch, SessionResourceResult, SessionResources};
use crate::system_reminder::TrustedSystemReminder;
use crate::thread::{AgentStatus, ThreadId, WorkState};

/// 组合回退所需的最小行为面。
///
/// [`SessionResources`] 的默认实现与 `peri-resources` 数据端口的默认实现共用这一份
/// 组合逻辑：两处各写一遍会让「先状态、再入队」的顺序与失败语义逐渐分叉。
#[async_trait]
pub trait ComposedSettlementOps: Send + Sync {
    async fn settlement_agent_status(&self, child: &ThreadId)
        -> SessionResourceResult<AgentStatus>;

    async fn settlement_update_status(
        &self,
        child: &ThreadId,
        status: AgentStatus,
    ) -> SessionResourceResult<()>;

    async fn settlement_set_work_state(
        &self,
        child: &ThreadId,
        state: WorkState,
    ) -> SessionResourceResult<()>;

    /// 按稳定 ID 查询目标 thread 的历史事实（只读、单条有界）。
    async fn settlement_history_contains(
        &self,
        thread_id: &ThreadId,
        message_id: MessageId,
    ) -> SessionResourceResult<bool>;

    async fn settlement_mq_enqueue(
        &self,
        target: &ThreadId,
        message: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome>;

    async fn settlement_append_reminder(
        &self,
        thread_id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<()>;

    async fn settlement_mq_receive(
        &self,
        thread_id: &ThreadId,
        selection: MqReceiveSelection,
    ) -> SessionResourceResult<MqReceiveOutcome>;
}

#[async_trait]
impl<T: SessionResources + ?Sized> ComposedSettlementOps for T {
    async fn settlement_agent_status(
        &self,
        child: &ThreadId,
    ) -> SessionResourceResult<AgentStatus> {
        Ok(self.load_session_meta(child).await?.agent_status)
    }

    async fn settlement_update_status(
        &self,
        child: &ThreadId,
        status: AgentStatus,
    ) -> SessionResourceResult<()> {
        self.update_session_meta(
            child,
            &SessionMetaPatch {
                status: Some(status),
                ..Default::default()
            },
        )
        .await
    }

    async fn settlement_set_work_state(
        &self,
        child: &ThreadId,
        state: WorkState,
    ) -> SessionResourceResult<()> {
        self.set_work_state(child, state).await
    }

    async fn settlement_history_contains(
        &self,
        thread_id: &ThreadId,
        message_id: MessageId,
    ) -> SessionResourceResult<bool> {
        self.mq_history_contains(thread_id, message_id).await
    }

    async fn settlement_mq_enqueue(
        &self,
        target: &ThreadId,
        message: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        self.mq_enqueue(target, message).await
    }

    async fn settlement_append_reminder(
        &self,
        thread_id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<()> {
        self.append_reminder_if_absent(thread_id, message_id, reminder)
            .await
            .map(|_| ())
    }

    async fn settlement_mq_receive(
        &self,
        thread_id: &ThreadId,
        selection: MqReceiveSelection,
    ) -> SessionResourceResult<MqReceiveOutcome> {
        self.mq_receive(thread_id, selection).await
    }
}

/// [`super::SessionResources::finalize_child_terminal`] 的回退：终态 → 工作意图 → 结果接纳。
///
/// 幂等判据是「这次结算是否已落地」：child 已是终态且结果 ID 已在目标 thread 的历史里
/// 时直接返回成功，不写状态、不写 `work_state`、不入队结果。
pub async fn composed_finalize_child_terminal<R>(
    resources: &R,
    child: &ThreadId,
    status: AgentStatus,
    work_state: WorkState,
    target: &ThreadId,
    message: MqInput,
) -> SessionResourceResult<MqEnqueueOutcome>
where
    R: ComposedSettlementOps + ?Sized,
{
    if let Some(outcome) = already_settled(resources, child, target, &message).await? {
        return Ok(outcome);
    }
    resources.settlement_update_status(child, status).await?;
    resources
        .settlement_set_work_state(child, work_state)
        .await?;
    resources.settlement_mq_enqueue(target, message).await
}

/// [`super::SessionResources::settle_retained_mq`] 的回退：先写提醒，再按快照水位删除 MQ 行。
pub async fn composed_settle_retained_mq<R>(
    resources: &R,
    thread_id: &ThreadId,
    reminders: &[RetainedReminder],
    rows: &[MqEntry],
) -> SessionResourceResult<()>
where
    R: ComposedSettlementOps + ?Sized,
{
    for reminder in reminders {
        resources
            .settlement_append_reminder(thread_id, reminder.id, &reminder.reminder)
            .await?;
    }
    if rows.is_empty() {
        return Ok(());
    }
    let selection = MqReceiveSelection {
        items: rows
            .iter()
            .cloned()
            .map(|entry| MqReceiveItem {
                entry,
                disposition: MqReceiveDisposition::Suppress,
            })
            .collect(),
        purpose: MqReceivePurpose::NonModelSettlement,
        execution: ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: crate::identity::AttemptId::new(),
        },
    };
    resources
        .settlement_mq_receive(thread_id, selection)
        .await
        .map(|_| ())
}

/// 这次结算已落地的成功结果（no-op）；否则返回 `None` 走原有写入路径。
///
/// 组合实现只能按稳定 ID 查历史：MQ 行没有单条等值查询，这一档无法判断结果是否仍在
/// 待发队列。因此只吞掉「结果已移交历史」的重复；消息仍在 MQ 时的同 ID 重放继续交给
/// 存储的幂等/冲突判定（同正文 `Existing`，不同正文 `Conflict`）。本机 SQLite 覆写
/// 同时查两张表，不受此限制。
async fn already_settled<R>(
    resources: &R,
    child: &ThreadId,
    target: &ThreadId,
    message: &MqInput,
) -> SessionResourceResult<Option<MqEnqueueOutcome>>
where
    R: ComposedSettlementOps + ?Sized,
{
    let current = resources.settlement_agent_status(child).await?;
    if !current.is_terminal() {
        return Ok(None);
    }
    if !resources
        .settlement_history_contains(target, message.id)
        .await?
    {
        return Ok(None);
    }
    Ok(Some(MqEnqueueOutcome {
        disposition: MqEnqueueDisposition::Existing,
        entry: MqEntry {
            id: message.id,
            // 这次调用没有写入任何行：entry 只是请求投影，不承诺这是持久入队时间。
            enqueued_at: String::new(),
            kind: message.kind,
            source: message.source.clone(),
            payload: message.payload.clone(),
            policy: message.policy.clone(),
            original_draft: message.original_draft.clone(),
        },
    }))
}
