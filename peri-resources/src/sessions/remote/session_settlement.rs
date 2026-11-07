//! 远端 child 终态结算的入口。
//!
//! 单批语句顺序与终态守卫由 [`super::session_mq`] 与 [`super::session_mq::SETTLE_CHILD_STATE_SQL`]
//! 承担；这里只保留 `SessionDataPort` 覆写所需的方法体，避免 `session_mq.rs` 与
//! `session_data.rs` 继续变长。

use peri_acp_types::session::{MqEnqueueOutcome, MqEntry, MqInput, RetainedReminder};
use peri_acp_types::session_resources::{
    SessionResourceErrorKind, SessionResourceResult, TerminalSettlementOutcome,
};
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};
use turso_serverless::Value;

use crate::sessions::failure::conflict;
use crate::sessions::mq_rows::{
    DELETE_THREAD_MQ_ROW_SQL, INSERT_MESSAGE_SQL, REFRESH_THREAD_COUNT_SQL,
};

use super::session_data::{invalid_input, not_found, RemoteSessionData};
use super::session_history::check_reminder_row;
use super::session_mq::{guard_session_statement, SETTLE_CHILD_STATE_SQL};
use super::sql::StatementSpec;

impl RemoteSessionData {
    /// 关闭结算的提醒与 MQ 删除共用一个托管事务，未知结果保留为未知。
    pub(super) async fn settle_retained_mq_atomic(
        &self,
        thread: &ThreadId,
        reminders: &[RetainedReminder],
        rows: &[MqEntry],
    ) -> SessionResourceResult<()> {
        self.ensure_writable().await?;
        let mut effects = vec![guard_session_statement(thread)];
        let mut inputs = vec![thread.to_string()];
        for reminder in reminders {
            let payload = PersistedPayload::SystemReminder {
                id: reminder.id,
                reminder: reminder.reminder.clone(),
            };
            let content = serialize_persisted_payload(&payload)
                .map_err(|error| invalid_input(&error.to_string()))?;
            let expected = match self.read_reminder_row(reminder.id).await? {
                Some(existing) => {
                    check_reminder_row(thread, reminder.id, &content, existing.clone())?;
                    existing.1
                }
                None => content.clone(),
            };
            let id = reminder.id.as_uuid().to_string();
            inputs.extend([id.clone(), content.clone()]);
            // 批内必须仍是已核对的同一内容，防止读写之间发生 ID 碰撞。
            effects.push(StatementSpec::new(
                "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE EXISTS (
                    SELECT 1 FROM messages WHERE message_id = ?1
                    AND (thread_id <> ?2 OR content <> ?3))",
                vec![
                    Value::Text(id.clone()),
                    Value::Text(thread.to_string()),
                    Value::Text(expected),
                ],
            ));
            effects.push(StatementSpec::new(
                INSERT_MESSAGE_SQL,
                vec![
                    Value::Text(id),
                    Value::Text(thread.to_string()),
                    Value::Text(crate::sessions::canonical::payload_role(&payload).to_owned()),
                    Value::Text(content),
                ],
            ));
        }
        for row in rows {
            let id = row.id.as_uuid().to_string();
            inputs.extend([id.clone(), row.enqueued_at.clone()]);
            effects.push(StatementSpec::new(
                DELETE_THREAD_MQ_ROW_SQL,
                vec![
                    Value::Text(thread.to_string()),
                    Value::Text(id),
                    Value::Text(row.enqueued_at.clone()),
                ],
            ));
        }
        effects.push(StatementSpec::new(
            REFRESH_THREAD_COUNT_SQL,
            vec![
                Value::Text(thread.to_string()),
                Value::Text(peri_time::now_utc_rfc3339()),
            ],
        ));
        self.commit_effects("settle_retained_mq", &inputs, effects, thread)
            .await?;
        Ok(())
    }

    /// 终态只写一次的远端结算：批内只在 child 仍为 `active` 时插入结果并写终态。
    ///
    /// 幂等与冲突归类见 [`super::session_mq`] 的 `finalize_child_terminal_mq`。
    pub(super) async fn finalize_child_terminal_settlement(
        &self,
        child: &ThreadId,
        status: AgentStatus,
        work_state: WorkState,
        target: &ThreadId,
        message: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        self.finalize_child_terminal_mq(child, status, work_state, target, message)
            .await
    }

    /// 无结果终态：单批条件写 child 终态与工作意图。
    ///
    /// 终态更新带 `agent_status = 'active'` 守卫，后到的结算不覆盖已提交终态；批内
    /// 会话守卫拒绝不存在的 child。批后按更新行数分类：1 行 = 本次写入；0 行时一致
    /// 读取既有终态返回 no-op，仍为 `active` 说明状态不可解释，按冲突失败而不是
    /// 宣称成功。
    pub(super) async fn settle_terminal_without_result_settlement(
        &self,
        child: &ThreadId,
        status: AgentStatus,
        work_state: WorkState,
    ) -> SessionResourceResult<TerminalSettlementOutcome> {
        self.ensure_writable().await?;
        let effects = vec![
            guard_session_statement(child),
            StatementSpec::new(
                SETTLE_CHILD_STATE_SQL,
                vec![
                    Value::Text(status.as_str().to_owned()),
                    Value::Text(peri_time::now_utc_rfc3339()),
                    Value::Text(work_state.as_str().to_owned()),
                    Value::Text(child.as_str().to_owned()),
                ],
            ),
        ];
        let inputs = vec![
            child.as_str().to_owned(),
            status.as_str().to_owned(),
            work_state.as_str().to_owned(),
        ];
        match self
            .commit_effects("settle_terminal_without_result", &inputs, effects, child)
            .await
        {
            Ok(counts) if counts.get(1) == Some(&1) => Ok(TerminalSettlementOutcome::Settled),
            Ok(_) => match self.read_meta(child).await {
                Ok(meta) if meta.agent_status.is_terminal() => Ok(
                    TerminalSettlementOutcome::AlreadyTerminal(meta.agent_status),
                ),
                Ok(_) => Err(conflict(
                    "terminal settlement did not observe an active child row",
                )),
                Err(error) => Err(error),
            },
            Err(error) => {
                // 批内守卫的拒绝只说明输入不自洽；只有确认 child 不存在才降级成
                // NotFound，未决/传输失败保持原分类。
                if matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. })
                    && !self.exists(child).await.unwrap_or(true)
                {
                    return Err(not_found());
                }
                Err(error)
            }
        }
    }
}
