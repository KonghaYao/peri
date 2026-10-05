use std::sync::{atomic::AtomicBool, Arc};

use super::StageContextBuilder;

/// 异步传输控制（run_react_loop idle 等待及完成提醒的只读后台活动判断）
#[derive(Clone)]
pub struct AsyncContext {
    pub idle_wait_enabled: bool,
    pub idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// 有界等待到期且仍有未结算任务时返回交接快照（只返回一次）。
    /// `idle_should_wait` 返回 false 后由 loop 退出路径消费。
    pub pending_handoff: Option<
        Arc<dyn Fn() -> Option<crate::agent::async_tasks::handoff::PendingHandoff> + Send + Sync>,
    >,
    /// 当前有界等待窗口的绝对截止时刻；idle 挂起据此设置定时器，保证
    /// 「无外部唤醒时到点退出」（`None` = 当前未在等待）。
    pub handoff_deadline: Option<Arc<dyn Fn() -> Option<tokio::time::Instant> + Send + Sync>>,
    /// Registry lifecycle signal. This is only a wake source; Receive re-checks
    /// the registry's active count after every notification.
    pub idle_registry: Option<tokio::sync::watch::Receiver<u64>>,
    /// 会话级 idle-suspended 标志（宿主 SessionAccessPort 注入的共享 Arc）。
    ///
    /// run_react_loop 在 await_wake 挂起期间置 true、醒来/取消时复位。
    /// 宿主 `dispatch_prompt_turn` 读取此标志把挂起期间到达的用户 prompt
    /// 注入 inbox（Prompt + wake），让挂起的 loop 立即醒来消费，而不是在
    /// per-session prompt lock 上阻塞至当前 turn 完成。
    pub idle_suspended_flag: Option<Arc<AtomicBool>>,
}

impl StageContextBuilder {
    pub fn with_idle_waiting(mut self) -> Self {
        self.async_ctx.idle_wait_enabled = true;
        self
    }

    pub fn with_idle_registry(mut self, receiver: tokio::sync::watch::Receiver<u64>) -> Self {
        self.async_ctx.idle_registry = Some(receiver);
        self
    }

    /// 设置 idle 时是否应该 await_wake 的判断 closure。
    /// 返回 true → 主 agent 有未完成异步任务，需要 await_wake 等结果续跑。
    /// 返回 false → 直接退出 loop，避免正常对话 loading 卡死。
    pub fn with_idle_should_wait(mut self, probe: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.async_ctx.idle_should_wait = Some(probe);
        self
    }

    /// 有界等待到期时的交接快照探针（见 [`AsyncContext::pending_handoff`]）。
    pub fn with_pending_handoff(
        mut self,
        probe: Arc<
            dyn Fn() -> Option<crate::agent::async_tasks::handoff::PendingHandoff> + Send + Sync,
        >,
    ) -> Self {
        self.async_ctx.pending_handoff = Some(probe);
        self
    }

    /// 有界等待窗口的截止时刻探针（idle 挂起据此设置定时器）。
    pub fn with_handoff_deadline(
        mut self,
        probe: Arc<dyn Fn() -> Option<tokio::time::Instant> + Send + Sync>,
    ) -> Self {
        self.async_ctx.handoff_deadline = Some(probe);
        self
    }

    /// 设置会话级 idle-suspended 标志（await_wake 挂起期间置 true；宿主
    /// `dispatch_prompt_turn` 据此把挂起期间到达的用户 prompt 注入 inbox）。
    pub fn with_idle_suspended_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.async_ctx.idle_suspended_flag = Some(flag);
        self
    }
}
