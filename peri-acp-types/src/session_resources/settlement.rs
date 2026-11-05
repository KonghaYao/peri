//! 跨行为原子操作的**回退组合**。
//!
//! 本机 SQLite 用单个事务覆写三条结算；远端存储用单个托管事务批复写
//! `finalize_child_terminal` 与 `settle_terminal_without_result`，`settle_retained_mq`
//! 尚未覆写、仍走这里的组合回退（已记录的限制，不是原子实现）。无事务后端也走组合
//! 实现。差异只在崩溃窗口：组合实现保证「失败前不宣称完成」，但进程在两步之间退出
//! 仍可能留下已写的终态或已写的提醒。调用方因此不得把这里的成功当成事务提交的证据，
//! 也不得依赖它做补偿。

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

/// 无结果终态写入的结果：本次是否真的写入，以及既有终态是什么。
///
/// `AlreadyTerminal` 是重复收尾的幂等 no-op，不表示结算失败；调用方不得再补写
/// 另一组终态/工作意图。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalSettlementOutcome {
    /// 本次调用把 child 从 `active` 写为终态，并写入对应的工作意图。
    Settled,
    /// child 已有终态；本次调用没有改写任何列。
    AlreadyTerminal(AgentStatus),
}

/// [`super::SessionResources::settle_terminal_without_result`] 的组合回退。
///
/// **降级实现**：先读状态，已是终态则直接 no-op；否则按「工作意图 → 终态」逐步提交。
/// 这个顺序保证失败时不会出现契约禁止的 `Done + working`，但两个行为不在同一个
/// 提交里：进程在两步之间退出可能留下 `Active + 目标工作意图`，并发写者也不能靠
/// 单次条件更新互斥。调用方不得把 `Settled` 当成原子提交的证据。
pub async fn composed_settle_terminal_without_result<R>(
    resources: &R,
    child: &ThreadId,
    status: AgentStatus,
    work_state: WorkState,
) -> SessionResourceResult<TerminalSettlementOutcome>
where
    R: ComposedSettlementOps + ?Sized,
{
    let current = resources.settlement_agent_status(child).await?;
    if current.is_terminal() {
        return Ok(TerminalSettlementOutcome::AlreadyTerminal(current));
    }
    resources
        .settlement_set_work_state(child, work_state)
        .await?;
    resources.settlement_update_status(child, status).await?;
    Ok(TerminalSettlementOutcome::Settled)
}

/// [`super::SessionResources::finalize_child_terminal`] 的回退：终态 → 工作意图 → 结果接纳。
///
/// 幂等判据是「这次结算是否已落地」：child 已是终态且结果 ID 已在目标 thread 的历史里
/// 时直接返回成功，不写状态、不写 `work_state`、不入队结果。
///
/// **降级实现**：三步不在同一个提交里，且状态先写；`work_state` 写入失败会留下
/// `Done + working`，后续步骤之间退出也可能留下已写终态但没有结果行。调用方不得把
/// 这里的成功当成原子提交的证据，也不得据此做补偿。具备批事务能力的后端必须覆写为
/// 单批（本机 SQLite 与远端 `finalize_child_terminal` 均如此）。
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
///
/// 本机 SQLite 覆写为单事务；远端尚无托管批覆写，当前生产路径仍落到这里，因此两步
/// 之间退出可能留下已写提醒与未删行，不得被当成关闭结算的原子提交证据。
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
