//! child 终态结算：终态、工作意图与结果消息的 MQ 接纳共用一次提交。
//!
//! 后台 task 是唯一的终态写入者。三次独立写入之间崩溃会留下
//! 「子任务已结束、父会话没收到结果」（`docs/design/session-durability.md` §5.3）；
//! 后端一次提交，未实现原子操作则返回 Unsupported。

use std::sync::Arc;

use peri_acp_types::session::MqInput;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};

use crate::agent::events::BackgroundTaskResult;

/// 没有结果可投递时的终态结算（后台执行体收尾）。
///
/// 终态与工作意图由资源侧同一次提交；未实现原子操作则拒绝。child 已是终态时是幂等 no-op，不覆盖既有终态。
pub(super) async fn settle_terminal_without_result(
    store: &dyn SessionResources,
    child: &ThreadId,
    status: AgentStatus,
) -> Result<(), String> {
    store
        .settle_terminal_without_result(child, status, WorkState::after_agent_terminal(status))
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// 终态结果要接纳到的 thread：通常是父会话，恢复期可能是 child 自己。
#[derive(Clone)]
pub(super) struct TerminalMqRoute {
    pub(super) store: Arc<dyn SessionResources>,
    pub(super) target: ThreadId,
}

impl TerminalMqRoute {
    pub(super) async fn settle_message(
        &self,
        child: &str,
        status: AgentStatus,
        message: &MqInput,
    ) -> Result<(), String> {
        self.store
            .finalize_child_terminal(
                &ThreadId::from(child),
                status,
                WorkState::after_agent_terminal(status),
                &self.target,
                message.clone(),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub(super) async fn settle(
        &self,
        child: &str,
        status: AgentStatus,
        result: &BackgroundTaskResult,
    ) -> Result<(), String> {
        let message = settlement_message(result);
        self.settle_message(child, status, &message).await
    }
}

/// 结果消息与常规投递共用同一条规范投影（稳定身份 = 重放幂等的依据）。
fn settlement_message(result: &BackgroundTaskResult) -> peri_acp_types::session::MqInput {
    crate::session::async_router::AsyncRouter::background_result_message(result, BgTaskKind::Agent)
        .into_mq_input()
}

#[cfg(test)]
#[path = "terminal_settlement_test.rs"]
mod tests;
