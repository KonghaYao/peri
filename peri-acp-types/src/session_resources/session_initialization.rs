//! Unpublished session creation lifecycle.

use super::{FrozenSnapshotBytes, SessionResourceResult};
use crate::thread::ThreadId;
use async_trait::async_trait;
use std::sync::Arc;

/// 未发布创建的句柄（J2 两阶段的第二阶段）：frozen 一次性提交，或整条草稿被撤销。
///
/// 语义（与失败补偿矩阵同源）：
///
/// - 提交是 **write-once CAS**：仅当这条 identity 从未提交过 frozen 时生效；重复调用返回
///   typed 冲突（[`super::SessionResourceErrorKind::Conflict`]），不覆盖已提交字节；
/// - 提交前检查未决持久化，执行唯一性由 peri-sdk 负责；
/// - [`Self::abandon`] 只撤销**未提交**的草稿（幂等）；已提交 frozen 的草稿必须拒绝删除
///   ——那会销毁一个「已定稿、未发布」的合法中间态；
#[async_trait]
pub trait SessionInitialization: Send + Sync {
    fn thread_id(&self) -> &ThreadId;

    /// 一次性提交 frozen：仅当该 identity 从未提交过；重复调用返回 typed 冲突，不覆盖。
    async fn commit_frozen(&self, frozen: &FrozenSnapshotBytes) -> SessionResourceResult<()>;

    /// 撤销未发布的创建（幂等）；已提交后调用返回 typed 冲突，不删除。
    async fn abandon(self: Arc<Self>) -> SessionResourceResult<()>;
}
