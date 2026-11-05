//! 远端 child 终态结算的入口。
//!
//! 单批语句顺序与终态守卫由 [`super::session_mq`] 与 [`super::session_mq::SETTLE_CHILD_STATE_SQL`]
//! 承担；这里只保留 `SessionDataPort` 覆写所需的方法体，避免 `session_mq.rs` 与
//! `session_data.rs` 继续变长。

use peri_acp_types::session::{MqEnqueueOutcome, MqInput};
use peri_acp_types::session_resources::{
    SessionResourceErrorKind, SessionResourceResult, TerminalSettlementOutcome,
};
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};
use turso_serverless::Value;

use crate::sessions::failure::conflict;

use super::session_data::{not_found, RemoteSessionData};
use super::session_mq::{guard_session_statement, SETTLE_CHILD_STATE_SQL};
use super::sql::StatementSpec;

impl RemoteSessionData {
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
