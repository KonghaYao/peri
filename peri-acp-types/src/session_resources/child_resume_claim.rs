//! Claimed child-session resume lifecycle.

use super::SessionResourceResult;
use crate::thread::AgentStatus;
use async_trait::async_trait;

/// 子会话 resume 认领 handle。
///
/// 认领期间的 active/原状态/终态由资源内部保存，调用方只提交领域结果，不拼补偿
/// 写入、不接触事务或重试令牌。
#[async_trait]
pub trait ChildResumeClaim: Send + Sync {
    /// 认领成功并开始运行。
    async fn mark_running(&self) -> SessionResourceResult<()>;
    /// 移交后台执行（仍属本次认领）；完成后调用方才可放行后台执行。
    async fn hand_off_to_background(&self) -> SessionResourceResult<()>;
    /// 认领后准备失败：恢复到认领前的状态，不留 active 残留。
    /// 移交失败或移交后启动被取消也可补偿，但调用方须确保后台执行尚未放行。
    async fn mark_failed(&self) -> SessionResourceResult<()>;
    /// 认领终止（取消/宿主退出）：恢复到认领前的状态。
    async fn mark_terminated(&self) -> SessionResourceResult<()>;

    /// 认领终止并提交领域终态，返回本次是否仍拥有该 child 的写入权。
    ///
    /// 实现必须把「恢复到认领前」与终态写入放在同一个归属校验内完成：返回
    /// `Ok(false)` 表示新认领已接管该 child，调用方不得再写终态。
    async fn finish_with_status(&self, status: AgentStatus) -> SessionResourceResult<bool>;
}
