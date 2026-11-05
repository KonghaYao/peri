//! child 终态结算：终态、工作意图与结果消息的 MQ 接纳共用一次提交。
//!
//! 后台 task 与 sync 交接 worker 共用此路由。三次独立写入之间崩溃会留下
//! 「子任务已结束、父会话没收到结果」（`docs/design/session-durability.md` §5.3）；
//! 具备事务的后端一次提交，组合后端只保证失败前不宣称完成。

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use peri_acp_types::session::MqInput;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};

use crate::agent::events::BackgroundTaskResult;
use crate::session::async_result_mailbox::AsyncResultMailbox;

const IN_EXECUTION: u8 = 0;
const HANDED_OFF: u8 = 1;
const SETTLED: u8 = 2;

/// 终态写入的进程内归属：执行体与 handoff worker 只能有一个提交终态。
///
/// 没有这个 CAS，「执行先结束 / 交接回执后到」的竞态会同时写两次终态；
/// 也不需要为此新增持久相位（`docs/design/session-durability.md` §5.3）。
pub(super) struct TerminalOwnership {
    state: AtomicU8,
    /// wrapper future 是否仍可能接管；Drop 后执行体必须自己收尾。
    wrapper_alive: AtomicBool,
    /// handoff worker 已被准入；此后终态只由 worker 的 finalize 提交。
    worker_admitted: AtomicBool,
    /// wrapper 已放弃交接（worker 准入被拒），由 wrapper 与执行体竞争终态写入。
    handoff_abandoned: AtomicBool,
}

impl TerminalOwnership {
    pub(super) fn new() -> Self {
        Self {
            state: AtomicU8::new(IN_EXECUTION),
            wrapper_alive: AtomicBool::new(true),
            worker_admitted: AtomicBool::new(false),
            handoff_abandoned: AtomicBool::new(false),
        }
    }

    /// handoff worker 接管终态；执行体已先收尾时返回 false，调用方不得 detach。
    pub(super) fn claim_handoff(&self) -> bool {
        self.transition(IN_EXECUTION, HANDED_OFF)
    }

    /// 准入成功：worker 已成唯一终态写入者。
    pub(super) fn mark_worker_admitted(&self) {
        self.worker_admitted.store(true, Ordering::Release);
    }

    /// 准入被拒：wrapper 放弃交接，执行体与 wrapper 以同一 CAS 决定唯一写入者。
    pub(super) fn abandon_handoff(&self) {
        self.handoff_abandoned.store(true, Ordering::Release);
    }

    /// 无 worker 且 wrapper 已退出/放弃时，执行体接回终态写入。
    pub(super) fn reclaim_abandoned_handoff(&self) -> bool {
        if self.worker_admitted.load(Ordering::Acquire) {
            return false;
        }
        if !self.handoff_abandoned.load(Ordering::Acquire)
            && self.wrapper_alive.load(Ordering::Acquire)
        {
            return false;
        }
        self.transition(HANDED_OFF, SETTLED)
    }

    /// 执行体自己写终态；handoff 已接管时返回 false，由 worker 的 finalize 负责。
    pub(super) fn claim_settlement(&self) -> bool {
        self.transition(IN_EXECUTION, SETTLED)
    }

    /// 交接已提交但 wrapper 还没决定：执行体先让位，避免抢在 detach 前写终态。
    pub(super) fn should_yield_handoff(&self) -> bool {
        self.wrapper_alive.load(Ordering::Acquire)
            && self.state.load(Ordering::Acquire) == IN_EXECUTION
    }

    pub(super) fn wrapper_finished(&self) {
        self.wrapper_alive.store(false, Ordering::Release);
    }

    fn transition(&self, from: u8, to: u8) -> bool {
        self.state
            .compare_exchange(from, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// 没有结果可投递时的同步终态结算（无 detach / worker 准入被拒 / 执行体收尾）。
///
/// 终态与工作意图由资源侧同一次提交；无事务后端按「工作意图 → 终态」组合并显式
/// 降级。child 已是终态时是幂等 no-op，不覆盖既有终态。
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
    /// 父会话结果保护 owner；可达时必须在 finalize 前建立 Queued 责任。
    pub(super) mailbox: Option<Arc<AsyncResultMailbox>>,
}

impl TerminalMqRoute {
    pub(super) fn message(&self, result: &BackgroundTaskResult) -> MqInput {
        settlement_message(result)
    }

    /// 结果可见前先建立父 mailbox 的 Queued 责任（计划 A3）。
    ///
    /// 同一 fingerprint 的重试是幂等 `Duplicate`；不同内容会在可见性写入前失败。
    pub(super) fn accept(&self, message: &MqInput) -> Result<(), String> {
        let Some(mailbox) = &self.mailbox else {
            return Ok(());
        };
        let PersistedPayload::SystemReminder { id, reminder } = &message.payload else {
            return Ok(());
        };
        mailbox
            .accept(*id, reminder, message.source.clone())
            .map(|_| ())
    }

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
