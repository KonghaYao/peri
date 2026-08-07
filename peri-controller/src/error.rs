//! Controller 层边界错误（`docs/top-level.md` §9 错误模型：边界类型化，层内 anyhow）。

/// Controller 层边界错误。
///
/// 仅对边界可判定条件类型化；Runtime 边界错误逐层包 context（`#[source]`）。
/// cancel 属终止类语义：转发成功即返回 `Ok`，是否终止由 Agent 层判定
/// （§9：Agent 持有最终执行权，上层仅传递）。
#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    /// run Session 失败（Runtime 边界错误包 context，含 UnknownSession）。
    #[error("session {0} run failed: {1}")]
    RunFailed(String, #[source] peri_runtime::RuntimeError),
    /// cancel 转发失败（Runtime 边界错误包 context，含 UnknownSession）。
    #[error("cancel failed for session {0}: {1}")]
    CancelFailed(String, #[source] peri_runtime::RuntimeError),
    /// join 会话失败（Runtime 边界错误包 context，含 UnknownSession）。
    #[error("join failed for session {0}: {1}")]
    JoinFailed(String, #[source] peri_runtime::RuntimeError),
    /// 销毁会话失败（Runtime 边界错误包 context，含 UnknownSession/PersistFailed；
    /// 持久化失败时映射保留，重试安全）。
    #[error("destroy failed for session {0}: {1}")]
    DestroyFailed(String, #[source] peri_runtime::RuntimeError),
    /// 运行时输入注入失败（Runtime 边界错误包 context，含 UnknownSession/SubmitFailed）。
    #[error("inject failed for session {0}: {1}")]
    InjectFailed(String, #[source] peri_runtime::RuntimeError),
}

/// 事件订阅错误（[`Subscription::recv`] 流语义）。
///
/// §9 事件契约 Broadcast 交付类：慢消费者 lagging；`Lagged` 为可恢复错误
/// （可继续 recv），`Closed` 为终态（事件流终止）。
#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError {
    /// 慢消费者错过事件（可继续 recv；错过条数可观测）。
    #[error("event subscription lagged, skipped {0} events")]
    Lagged(u64),
    /// 事件流已终止（广播通道关闭）。
    #[error("event subscription closed")]
    Closed,
}
