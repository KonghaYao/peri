//! 后台 child 终态结算：终态、工作意图与结果消息的 MQ 接纳共用一次提交。
//!
//! 三次独立写入之间崩溃会留下「子任务已结束、父会话没收到结果」
//! （`docs/design/session-durability.md` §5.3）。提交失败时三条事实都不写，执行体
//! 按既有失败协议把结果交付给父会话。

use std::sync::Arc;

use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};

use crate::agent::events::BackgroundTaskResult;

/// 终态结果要接纳到的 thread：通常是父会话，恢复期可能是 child 自己。
#[derive(Clone)]
pub(super) struct TerminalMqRoute {
    pub(super) store: Arc<dyn SessionResources>,
    pub(super) target: ThreadId,
}

impl TerminalMqRoute {
    pub(super) async fn settle(
        &self,
        child: &str,
        status: AgentStatus,
        result: &BackgroundTaskResult,
    ) -> Result<(), String> {
        self.store
            .finalize_child_terminal(
                &ThreadId::from(child),
                status,
                WorkState::after_agent_terminal(status),
                &self.target,
                settlement_message(result),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
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
