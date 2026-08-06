//! Runtime 层边界错误（`docs/top-level.md` §9 错误模型：边界类型化，层内 anyhow）。

/// Runtime 层边界错误。
///
/// 仅对边界可判定条件类型化；Agent 侧句柄实现的细节错误经 anyhow 穿透，
/// 在本边界逐层包 context。
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// 未注册的 session（映射查找失败：run/cancel/destroy/stamp 共同路径）。
    #[error("unknown session: {0}")]
    UnknownSession(String),
    /// 重复注册（同 session_id 已存在；须先 destroy 旧实例再重建）。
    #[error("session already registered: {0}")]
    SessionAlreadyRegistered(String),
    /// 执行失败（run 语义，Agent 层错误经 anyhow 穿透到边界）。
    #[error("session {0} run failed: {1}")]
    RunFailed(String, #[source] anyhow::Error),
    /// 持久化事务收束失败（销毁阶段 5；映射保留，可重试销毁）。
    #[error("session {0} persist failed: {1}")]
    PersistFailed(String, #[source] anyhow::Error),
    /// 运行时输入注入失败（submit_input 语义；Agent 侧错误经 anyhow 穿透到边界）。
    #[error("session {0} submit input failed: {1}")]
    SubmitFailed(String, #[source] anyhow::Error),
}
