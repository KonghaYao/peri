//! 本机执行面端口 — 门面持有的**本机执行事实**行为接缝。
//!
//! 与 [`super::data::SessionDataPort`] 的分工是事实归属，不是实现细节：
//!
//! - 数据端口回答 canonical 会话数据（会话行、绑定字节、历史、frozen、父链）；
//! - 本端口回答**只可能由本机回答**的事：工作区发现与登记证据、执行代际、OS 锁、
//!   在途写入门禁、创建准入。lease 只在这里出现，数据端口里没有它。
//!
//! 只有唯一实现 [`LocalExecution`]（本机 SQLite）：远端组合的 canonical 数据在远端，
//! 但执行事实（代际、锁、owner）仍只写在本机库。绑定字节与父链由数据端口提供——远端
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
    RecoveryRequiredDetails, ResolvedWorkspace, SessionBinding, SessionExecutionLease,
};

use super::sqlite_store::{ExclusiveExecutionGuard, ExecutionLease, ExecutionWriteGuard};

/// 一次撤销补偿（放弃未发布创建时由门面提供的唯一副作用）。
///
/// 调用方只给「撤销这次创建」这一件事，执行/锁顺序仍由本端口实现决定：补偿先成功，
/// 才关闭准入并释放 OS 锁。
pub(in crate::sessions) type RevokeEffect<'a> =
    Pin<Box<dyn Future<Output = SessionResourceResult<()>> + Send + 'a>>;

/// 数据面给出的会话事实：绑定字节、这棵树有没有绑定、树根。
///
/// 三件都只可能由持有 canonical 数据的一侧回答（[`super::data::SessionDataPort::binding_of`] /
/// [`super::data::SessionDataPort::session_root`]）：远端组合里本机没有这条会话的任何行，本机
/// 执行面因此不查本机的 `threads` / `session_bindings`，只按调用方给出的值判定。
///
/// 用途是确定的：绑定字节用于取得所有权前的关系复核，`bound` 区分「无绑定历史（没有可保护
/// 的执行域）」与「有绑定但无活 owner（必须拒绝写入）」，`root` 定位子会话的 owner——子会话
/// 由 root 的租约与它的关闭事务统一持有，自己没有租约也不写 `execution_runs`。
pub(in crate::sessions) struct SessionFacts {
    /// 这条会话自己的绑定字节；没有绑定行时 `None`。
    pub binding: Option<SessionBinding>,
    /// 这棵**树**在数据面上有没有绑定：自身或 root 有绑定即算有。
    ///
    /// 两种来源都要看：接纳过的 legacy root 可以有自己没有绑定行的子会话，那些子会话的写入
    /// 同样落在 root 的执行域里，不能因为「自己无绑定」就当成无主放行。
    pub bound: bool,
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

    /// 本机来源证据是否足以把无绑定历史表达成 legacy（远端组合由门面固定为 `false`）。
    async fn legacy_confirmed(&self, id: &ThreadId) -> Result<bool>;

    // ── owner / dirty ──

    /// 本机执行代际事实（generation, clean）；不创建锁文件、不改状态。
    async fn execution_state(&self, id: &ThreadId) -> Result<Option<(i64, bool)>>;

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

    /// 解除精确代际的本机 dirty（CAS 在实现内部，不跨接口传递）。
    async fn reset_dirty(&self, target: &RecoveryRequiredDetails) -> Result<()>;

    // ── 创建准入 ──

    /// 新建会话的执行准入（本地塌缩：数据与执行代际一次提交）。
    ///
    /// 只有「数据与执行代际在同一个本机库」的组合调它；数据在另一端的组合先由数据端口
    /// 保存，再调 [`Self::admit_existing`]（见 `SessionDataHome`）。
    async fn create_session(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>>;

    /// 为「数据已完整保存、还没有执行代际」的会话建立准入（收敛，不是重建）。
    ///
    /// `binding` 是**数据面给出的绑定字节**（本机组合来自本机 `session_bindings`，远程组合
    /// 来自远端会话行）——会话是否存在由数据面回答，本机只做本机能回答的那部分：执行代际
    /// 的唯一性、sidecar 锁，以及「这组字节指向本机已登记的目录」这条复核。调用方必须先在
    /// 数据面证明会话已保存；本端口不查本机会话表来代它证明。
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
    /// 删除是完整生命周期行为，数据消失之后本机也不该留下任何属于它的执行事实：
    ///
    /// 1. 删掉本机 `execution_runs` 里这条 identity 的代际行。本机组合在删数据的同一事务里
    ///    已经删过（这里是幂等的空操作）；数据在**远端**的组合则靠这一步收敛——远端行消失
    ///    之后本机还留着一条代际行，那是一条没有对象的行。
    /// 2. 本进程若持有它的活 owner，按「数据已删除」的终态释放（`ExecutionLease::dispose_ownership`，
    ///    不做 clean CAS）。没有活 owner 不是错误：数据面已经删干净了，本机没有可结束的所有权。
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
