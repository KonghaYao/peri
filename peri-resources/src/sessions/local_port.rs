//! 本机执行面端口 — 门面持有的**本机执行事实**行为接缝。
//!
//! 与 [`super::data::SessionDataPort`] 的分工是事实归属，不是实现细节：
//!
//! - 数据端口回答 canonical 会话数据（会话行、绑定字节、历史、frozen、父链）；
//! - 本端口回答**只可能由本机回答**的事：工作区发现与登记证据、运行句柄、
//!   在途写入门禁、创建准入。lease 只在这里出现，数据端口里没有它。
//!
//! 本地由 [`LocalExecution`] 实现；Turso 由进程内 `RemoteExecution` 实现，
//! 不持有本地 SQLite。运行句柄及未知效果仅驻留当前实例。绑定字节与父链由数据端口提供——远端
//! 组合给的是远端会话行自带的 `binding_*` 列，本机组合给的是本机 `session_bindings`。
//! 本端口因此不查绑定行，只接受调用方给出的字节并做**本机复核**（目录证据、关系）。
//!
//! 门面不区分后端：它按同一个端口调用，组合层决定注入哪个数据端口。公开行为仍只有
//! 一套（门面的行为清单），没有为远程另建平行行为。

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use peri_acp_types::session_resources::{NewSession, SessionResourceResult};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{
    ResolvedWorkspace, SessionBinding, SessionExecutionLease, WorkspaceId,
};

use super::execution::{ExclusiveExecutionGuard, ExecutionLease, ExecutionWriteGuard};

/// 一次撤销补偿（放弃未发布创建时由门面提供的唯一副作用）。
///
/// 调用方只给「撤销这次创建」这一件事，收尾顺序由本端口实现决定：补偿先成功，
/// 才关闭本次运行句柄。
pub(in crate::sessions) type RevokeEffect<'a> =
    Pin<Box<dyn Future<Output = SessionResourceResult<()>> + Send + 'a>>;

/// 数据面沿父链确定的树根，用于共享当前运行实例的写入与关闭屏障。
pub(in crate::sessions) struct SessionFacts {
    /// 这条会话在树中的根，含自身。
    pub root: ThreadId,
}

/// 本机执行面端口。
///
/// 所有方法都是「本机事实」，没有一条会去远端写数据：远端写入由数据端口在门面编排下
/// 完成，本端口只在写完之后建立或复核本机执行资格。
#[async_trait]
pub(in crate::sessions) trait LocalExecutionPort: Send + Sync {
    /// 本机是否只读打开（只读时任何执行资格都不可得，历史仍可读）。
    fn is_read_only(&self) -> bool;

    // ── 发现与登记 ──

    /// 解析并登记本机执行目录。
    async fn resolve_workspace(&self, cwd: &Path) -> Result<ResolvedWorkspace>;

    /// 用调用方给出的绑定字节做本机复核：目录关系、关键文件对象，`full` 为真时再叠一次
    /// 完整发现快照比对（一次准入的权威复核）。
    ///
    /// 本机组合的字节来自本机 `session_bindings`，远端组合的字节来自远端会话行；两种组合
    /// 走的是同一套本机判定，因此复核结论不会因为数据在哪而不同。
    async fn validate_binding_value(
        &self,
        binding: &SessionBinding,
        full: bool,
    ) -> Result<ResolvedWorkspace>;

    /// Recheck immutable evidence loaded from the canonical store. The remote adapter
    /// uses this after restart; a missing snapshot must never be reconstructed.
    async fn validate_saved_binding(
        &self,
        binding: &SessionBinding,
        _snapshot: &str,
        _owner: WorkspaceId,
        full: bool,
    ) -> Result<ResolvedWorkspace> {
        self.validate_binding_value(binding, full).await
    }

    /// 本机来源证据是否足以把无绑定历史表达成 legacy（远端组合由门面固定为 `false`）。
    async fn legacy_confirmed(&self, id: &ThreadId) -> Result<bool>;

    /// 沿 root 关系找到活 owner；`None` 表示整棵树既没有绑定也没有活 owner。
    ///
    /// 树形事实由调用方从数据面给出（[`SessionFacts::root`]），本机不沿自己的 `threads` 上溯。
    async fn owner_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>>;

    /// 诊断读取：本进程是否持有这棵树的 owner（没有不构成错误）。
    async fn live_owner(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>>;

    /// 本进程当前持有的全部活 owner（关闭协调用；不跨进程探测）。
    fn live_leases(&self) -> Vec<Arc<ExecutionLease>>;

    /// 读侧写入准入（允许同 root 并发 mutation）。
    async fn write_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExecutionWriteGuard>>;

    /// 写侧写入准入（把「检查 + 写入」做成不可插入的区间）。
    async fn exclusive_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExclusiveExecutionGuard>>;

    /// 取得已有会话的执行所有权。
    ///
    /// 只有 root 能取得所有权（[`SessionFacts::root`] 就是它自己）；绑定关系用调用方给出的
    /// [`SessionFacts::binding`] 字节在本机复核一次（准入内不重复完整发现）。
    async fn acquire_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Arc<dyn SessionExecutionLease>>;

    // ── 创建准入 ──

    /// 新建会话的执行准入：事务保存 canonical 数据，再登记运行句柄。
    ///
    /// 只有数据在本机库的组合调它；数据在另一端的组合先由数据端口
    /// 保存，再调 [`Self::admit_existing`]（见 `SessionDataHome`）。
    async fn create_session(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>>;

    /// 未发布创建的第一阶段：事务保存 canonical 草稿，再登记运行句柄。
    ///
    /// 与 [`Self::create_session`] 同一形状，只把 frozen 值改为 `NULL`；返回的租约就是
    /// 本条 identity 的活 owner，frozen 提交（[`Self::commit_frozen`]）在同一事务内复核它。
    async fn begin_initialization(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>>;

    /// 一次性提交 frozen：排他门禁内复核 owner，事务内校验 binding 并执行 CAS。
    ///
    /// `lease` 必须是当前确切的活跃 Arc，且不存在未知写入效果；
    /// `UPDATE ... WHERE frozen_context IS NULL` 必须恰好命中一行，否则 typed 冲突且不写入。
    async fn commit_frozen(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
        frozen: &peri_acp_types::session_resources::FrozenSnapshotBytes,
    ) -> SessionResourceResult<()>;

    /// 提交前的本机资格复核：当前确切 Arc 仍活跃，且不存在未知写入效果。
    ///
    /// 数据在远端的组合用它把「本机执行事实」叠在远端 CAS 之前（远端组合没有同一个事务
    /// 可以承载本机 owner 判定）。
    async fn verify_initialization_owner(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()>;

    /// 半写草稿（`bound && frozen IS NULL`）的检测与清理。
    ///
    /// 判据不成立（无绑定行、或已提交 frozen）时返回 typed 冲突且不删除；本进程存在活
    /// 初始化句柄时同样拒绝（那不是崩溃残留）。
    async fn discard_incomplete_initialization(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 为数据已完整保存的会话登记运行句柄（收敛，不是重建）。
    ///
    /// `binding` 是**数据面给出的绑定字节**（本机组合来自本机 `session_bindings`，远程组合
    /// 来自远端会话行）；调用方已确认数据保存及机器环境可用。本端口仅登记运行句柄，
    /// 不按目录对象认领会话，也不查本机会话表代替数据面证明。
    async fn admit_existing(
        &self,
        id: &ThreadId,
        binding: &SessionBinding,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>>;

    /// 放弃一次未发布创建的所有权并执行补偿。
    async fn abandon_initialization(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
        revoke: RevokeEffect<'_>,
    ) -> SessionResourceResult<()>;

    // ── 删除后的收尾 ──

    /// 会话**数据已被删除**：结束这条 identity 的本机执行事实与所有权。
    ///
    /// 排空并关闭当前实例登记的确切 Arc，不清除未知效果，不更新持久执行状态。
    /// 没有登记句柄不是错误。
    ///
    /// 只由门面在数据面删除**返回成功之后**调用：本方法不判断数据在不在，也不把「行缺失」
    /// 当成删除的证据。删除失败时门面不会调它（所有权原样保留，调用方仍可重试或显式放弃）。
    async fn dispose_execution(&self, id: &ThreadId) -> SessionResourceResult<()>;

    /// 测试用：本机 SQLite 连接池。
    #[cfg(test)]
    fn sqlite_pool(&self) -> Option<&sqlx::SqlitePool> {
        None
    }
}
