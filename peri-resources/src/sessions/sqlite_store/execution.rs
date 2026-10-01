use super::database::SqliteSessionDatabase;
use crate::sessions::local_port::SessionFacts;
use anyhow::Result;
use async_trait::async_trait;
use peri_acp_types::{
    session_resources::{MutationOutcome, SessionResourceResult},
    thread::ThreadId,
    workspace::{SessionExecutionLease, WorkspaceError},
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(in crate::sessions) struct ExecutionLease {
    thread_id: ThreadId,
    active: AtomicBool,
    mutation_gate: Arc<tokio::sync::RwLock<()>>,
    mutation_uncertain: AtomicBool,
}

impl ExecutionLease {
    pub(super) fn new(thread_id: ThreadId) -> Self {
        Self {
            thread_id,
            active: AtomicBool::new(true),
            mutation_gate: Arc::new(tokio::sync::RwLock::new(())),
            mutation_uncertain: AtomicBool::new(false),
        }
    }

    /// 本次所有权是否仍接受新写入（关闭不可逆）。
    pub(in crate::sessions) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// 是否存在无法证明终态的写入。
    pub(in crate::sessions) fn is_uncertain(&self) -> bool {
        self.mutation_uncertain.load(Ordering::Acquire)
    }

    /// 有界排空屏障：取写侧门禁再释放，证明此刻没有已准入写入在途。
    ///
    /// 调用方负责超时：等待外部 future 时不持普通全局互斥量。
    pub(in crate::sessions) async fn wait_for_in_flight(&self) {
        let _gate = self.mutation_gate.clone().write_owned().await;
    }

    /// 会话数据已删除时排空并关闭本次句柄，不清除未知效果。
    pub(in crate::sessions) async fn dispose_ownership(&self) {
        let _writes = self.mutation_gate.clone().write_owned().await;
        self.active.store(false, Ordering::Release);
    }
}

/// The guard retains both the lease and admission lock until the complete SQL operation finishes.
/// A cancelled mutation retains uncertain effects even if SQLx still has a queued command.
pub(in crate::sessions) struct ExecutionWriteGuard {
    lease: Arc<ExecutionLease>,
    _gate: tokio::sync::OwnedRwLockReadGuard<()>,
    completed: bool,
}

impl ExecutionWriteGuard {
    pub(in crate::sessions) fn finish(mut self) {
        self.completed = true;
    }
}

impl Drop for ExecutionWriteGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.lease.mutation_uncertain.store(true, Ordering::Release);
        }
    }
}

/// 排他写入范围：持有同一 owner 的写侧门禁。
///
/// 与 [`ExecutionWriteGuard`] 的区别是并发语义：读侧门禁允许同 root 的多个 mutation
/// 并发（SQLite 自己保证事务串行），写侧门禁用来把「检查 + 写入」做成一段不可插入的
/// 区间（child resume 认领的状态检查与写入、未发布创建的撤销）。
pub(in crate::sessions) struct ExclusiveExecutionGuard {
    lease: Arc<ExecutionLease>,
    _gate: tokio::sync::OwnedRwLockWriteGuard<()>,
    completed: bool,
}

impl ExclusiveExecutionGuard {
    pub(in crate::sessions) fn finish(mut self) {
        self.completed = true;
    }
}

impl Drop for ExclusiveExecutionGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.lease.mutation_uncertain.store(true, Ordering::Release);
        }
    }
}

/// 事务性写入的效果边界：进入 `commit` 前可证明「未生效」，提交成功后是「已生效」，
/// 只有提交自身的失败落在证明之外。
///
/// `sqlx` 的 SQLite 事务在 `commit()` 失败后由连接回滚，但「回滚是否真的完成」不由
/// 调用方观察得到；这里把不可证明的那一小段标出来，让写入准入保持未决，而不是用
/// `Err` 冒充「没生效」。
#[derive(Default)]
pub(in crate::sessions) struct TransactionEffect {
    committing: bool,
    committed: bool,
}

impl TransactionEffect {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// 即将调用 `commit()`：此后失败不再能证明未生效。
    pub(super) fn enter_commit(&mut self) {
        self.committing = true;
    }

    /// `commit()` 已成功返回。
    pub(super) fn commit_succeeded(&mut self) {
        self.committing = false;
        self.committed = true;
    }

    /// 按效果结清准入：可证明「未生效」或「已生效」时才 `finish`；其余情况释放 guard
    /// 由 `Drop` 留下未决证据。
    pub(super) fn settle(&self, guard: Option<ExecutionWriteGuard>) {
        if !self.committing {
            if let Some(guard) = guard {
                guard.finish();
            }
        }
    }
}

#[async_trait]
impl SessionExecutionLease for ExecutionLease {
    fn thread_id(&self) -> &ThreadId {
        &self.thread_id
    }

    async fn mark_clean(&self) -> Result<()> {
        let _writes = self.mutation_gate.write().await;
        if self.mutation_uncertain.load(Ordering::Acquire) {
            anyhow::bail!("session persistence outcome is uncertain");
        }
        self.active.store(false, Ordering::Release);
        Ok(())
    }
}

impl ExecutionLease {
    /// 放弃本次所有权：等待已准入写入 → 执行补偿 → 关闭本次运行句柄。
    ///
    /// 只用于本次创建被撤销；不声明此前未知写入的效果。
    ///
    /// 已知失败不关闭句柄；取消或未知效果保留未决门禁，不能用失败冒充未生效。
    pub(in crate::sessions) async fn abandon_ownership<F, Fut, T>(
        self: &Arc<Self>,
        compensate: F,
    ) -> SessionResourceResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = SessionResourceResult<T>>,
    {
        // 补偿期间取写侧门禁：已准入的写入先结束，新写入等在这里。
        let gate = self.mutation_gate.clone().write_owned().await;
        if self.is_uncertain() {
            return Err(super::failure::conflict(
                "session persistence outcome is uncertain",
            ));
        }
        let guard = ExclusiveExecutionGuard {
            lease: Arc::clone(self),
            _gate: gate,
            completed: false,
        };
        let outcome = compensate().await;
        if outcome.is_ok() {
            self.active.store(false, Ordering::Release);
        }
        if !outcome
            .as_ref()
            .is_err_and(|error| error.effect() == MutationOutcome::Unknown)
        {
            guard.finish();
        }
        outcome
    }
}

impl SqliteSessionDatabase {
    pub(super) async fn acquire_execution_lease_impl(
        &self,
        id: &ThreadId,
        _facts: &SessionFacts,
    ) -> Result<Arc<dyn SessionExecutionLease>> {
        let lease = self.register_execution_lease(id)?;
        Ok(lease)
    }

    pub(super) fn register_execution_lease(&self, id: &ThreadId) -> Result<Arc<ExecutionLease>> {
        if self.read_only || self.pool.is_closed() {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let mut leases = self
            .execution_leases
            .lock()
            .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?;
        if let Some(lease) = leases.get(id) {
            if lease.is_uncertain() {
                anyhow::bail!("session persistence outcome is uncertain");
            }
            if lease.is_active() {
                return Ok(Arc::clone(lease));
            }
            let _closed = lease
                .mutation_gate
                .try_write()
                .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?;
        }
        let lease = Arc::new(ExecutionLease::new(id.clone()));
        leases.insert(id.clone(), Arc::clone(&lease));
        Ok(lease)
    }

    /// 本进程登记的这条 identity 的活 owner（精确 id，不沿父链上溯）。
    ///
    /// 上溯需要父链，而父链是数据面事实（远端组合里本机没有这条会话的行），因此本函数只回答
    /// 「本进程是否持有**这一条** identity 的租约」。子树归属由调用方按 [`SessionFacts::root`]
    /// 显式问 root（见 [`Self::owner_lease`]）。
    pub(super) fn registered_lease(&self, id: &ThreadId) -> Result<Option<Arc<ExecutionLease>>> {
        Ok(self
            .execution_leases
            .lock()
            .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?
            .get(id)
            .cloned())
    }

    /// 查找本进程登记的运行句柄；未登记不构成持久会话的写入认领门槛。
    ///
    /// 本函数不取门禁：调用方必须自己决定要读侧还是写侧门禁（见
    /// [`Self::require_execution_lease`] 与 [`Self::exclusive_execution_guard`]），
    /// 避免在同一个 async 任务里嵌套两次加锁。
    pub(super) async fn owner_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        if let Some(lease) = self.registered_lease(id)? {
            return Ok(Some(lease));
        }
        if facts.root != *id {
            if let Some(lease) = self.registered_lease(&facts.root)? {
                return Ok(Some(lease));
            }
        }
        Ok(None)
    }

    /// 诊断读取：只回答「本进程是否持有这棵树的 owner」，不把「有绑定但无 owner」
    /// 当成错误——那正是可执行（尚未取得所有权）的常态。
    pub(super) async fn live_owner_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        if let Some(lease) = self.registered_lease(id)? {
            return Ok(Some(lease));
        }
        if facts.root != *id {
            return self.registered_lease(&facts.root);
        }
        Ok(None)
    }

    pub(super) async fn require_execution_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExecutionWriteGuard>> {
        if self.read_only || self.pool.is_closed() {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let Some(lease) = self.owner_lease(id, facts).await? else {
            return Ok(None);
        };
        self.write_guard_for(lease).await
    }

    /// 读侧写入准入（按已解析的 root 租约）：[`Self::owner_lease`] 之后的取门禁一步，
    /// 门禁挂在 root 的租约上（子会话与 root 共享同一条门禁）。
    pub(super) async fn write_guard_for(
        &self,
        lease: Arc<ExecutionLease>,
    ) -> Result<Option<ExecutionWriteGuard>> {
        let gate = lease.mutation_gate.clone().read_owned().await;
        // Close may have won while admission waited behind its write lock.
        if !lease.active.load(Ordering::Acquire) {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        if lease.mutation_uncertain.load(Ordering::Acquire) {
            anyhow::bail!("session persistence outcome is uncertain");
        }
        Ok(Some(ExecutionWriteGuard {
            lease,
            _gate: gate,
            completed: false,
        }))
    }

    /// 排他范围：与 [`Self::require_execution_lease`] 相同的准入判定（同一套数据面事实），
    /// 但取写侧门禁。
    pub(super) async fn exclusive_execution_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExclusiveExecutionGuard>> {
        if self.read_only || self.pool.is_closed() {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let Some(lease) = self.owner_lease(id, facts).await? else {
            return Ok(None);
        };
        self.exclusive_guard_for(lease).await
    }

    /// 写侧写入准入（按已解析的 root 租约）：同 [`Self::write_guard_for`]，取写锁。
    pub(super) async fn exclusive_guard_for(
        &self,
        lease: Arc<ExecutionLease>,
    ) -> Result<Option<ExclusiveExecutionGuard>> {
        let gate = lease.mutation_gate.clone().write_owned().await;
        if !lease.active.load(Ordering::Acquire) {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        if lease.mutation_uncertain.load(Ordering::Acquire) {
            anyhow::bail!("session persistence outcome is uncertain");
        }
        Ok(Some(ExclusiveExecutionGuard {
            lease,
            _gate: gate,
            completed: false,
        }))
    }
}
