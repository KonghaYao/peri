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
}
