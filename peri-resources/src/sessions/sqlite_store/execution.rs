use super::database::SqliteSessionDatabase;
use crate::sessions::local_port::SessionFacts;
use anyhow::{Context, Result};
use async_trait::async_trait;
use peri_acp_types::{
    session_resources::SessionResourceResult,
    thread::ThreadId,
    workspace::{RecoveryRequiredDetails, SessionExecutionLease, WorkspaceError},
};
use sqlx::SqlitePool;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(in crate::sessions) struct ExecutionLease {
    thread_id: ThreadId,
    generation: i64,
    pool: SqlitePool,
    active: AtomicBool,
    mutation_gate: Arc<tokio::sync::RwLock<()>>,
    mutation_uncertain: AtomicBool,
}

impl ExecutionLease {
    pub(super) fn new(thread_id: ThreadId, generation: i64, pool: SqlitePool) -> Self {
        Self {
            thread_id,
            generation,
            pool,
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

    /// 结束本次所有权：会话**数据已被删除**时的收尾，不做 clean CAS。
    ///
    /// 与 [`SessionExecutionLease::mark_clean`] 的唯一区别是「不发声明」：代际行已随数据
    /// 删除，clean 这句话没有对象，硬写下 `clean = 1` 只会制造一条描述不存在会话的行。
    /// 先排空已准入写入，再不可逆地关闭本次运行句柄。
    ///
    /// 结束后本句柄仍是幂等终态：`mark_clean` 可直接成功返回，因此调用方
    /// 无需知道数据删除与所有权结束的先后。
    ///
    /// 只有调用方能给出「数据确实已删除」这个事实（门面在删除返回成功后调用），本方法
    /// 自己不做任何推测：它不查数据行，也不把「行缺失」当成删除的证据。
    pub(in crate::sessions) async fn dispose_ownership(&self) {
        let _writes = self.mutation_gate.clone().write_owned().await;
        self.active.store(false, Ordering::Release);
    }
}

/// The guard retains both the lease and admission lock until the complete SQL operation finishes.
/// A cancelled mutation leaves its run dirty even if SQLx still has a queued database command.
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
        sqlx::query("UPDATE execution_runs SET clean = 1 WHERE thread_id = ? AND generation = ?")
            .bind(self.thread_id.as_str())
            .bind(self.generation)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

impl ExecutionLease {
    /// 放弃本次所有权：等待已准入写入 → 执行补偿 → 关闭本次运行句柄。
    ///
    /// 只用于「本次创建被撤销」：数据行会被删除，因此不能走 `mark_clean` 的 clean CAS
    /// （那要求记录仍然存在）。
    ///
    /// 补偿失败时**不动**所有权状态：不关闭本次运行句柄，调用方仍持有这条会话并可
    /// 重试撤销或继续使用。反过来若先关闭准入再补偿，一次失败的补偿会把会话变成
    /// 「既没撤销、又不能再用」，那才是真正的半状态。
    pub(in crate::sessions) async fn abandon_ownership<F, Fut, T>(
        &self,
        compensate: F,
    ) -> SessionResourceResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = SessionResourceResult<T>>,
    {
        // 补偿期间取写侧门禁：已准入的写入先结束，新写入等在这里。
        let gate = self.mutation_gate.clone().write_owned().await;
        let outcome = compensate().await;
        if outcome.is_ok() {
            self.active.store(false, Ordering::Release);
            drop(gate);
        }
        outcome
    }
}

impl SqliteSessionDatabase {
    pub(super) async fn reset_dirty_execution_impl(
        &self,
        target: &RecoveryRequiredDetails,
    ) -> Result<()> {
        if self.read_only {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let updated = sqlx::query(
            "UPDATE execution_runs SET clean = 1 WHERE thread_id = ? AND generation = ? AND clean = 0",
        )
        .bind(target.thread_id.as_str())
        .bind(target.generation)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(WorkspaceError::RecoveryGenerationMismatch.into());
        }
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn acquire_execution_lease_impl(
        &self,
        id: &ThreadId,
        _facts: &SessionFacts,
    ) -> Result<Arc<dyn SessionExecutionLease>> {
        if self.read_only {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let key = id.clone();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let prior: Option<(i64, bool)> =
            sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?")
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        let generation = prior
            .map_or(Some(1), |(generation, _)| generation.checked_add(1))
            .context("execution generation exhausted")?;
        sqlx::query(
            "INSERT INTO execution_runs (thread_id, generation, clean) VALUES (?, ?, 0)
            ON CONFLICT(thread_id) DO UPDATE SET generation = excluded.generation, clean = 0",
        )
        .bind(id.as_str())
        .bind(generation)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let lease = Arc::new(ExecutionLease::new(
            id.clone(),
            generation,
            self.pool.clone(),
        ));
        self.execution_leases
            .lock()
            .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?
            .insert(key, Arc::downgrade(&lease));
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
            .and_then(std::sync::Weak::upgrade))
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

    /// 本机执行代际事实（generation, clean）；不创建锁文件、不改变状态。
    pub(super) async fn load_execution_state(&self, id: &ThreadId) -> Result<Option<(i64, bool)>> {
        Ok(
            sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?")
                .bind(id.as_str())
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// 删除这条 identity 的执行代际行（会话数据已删除时的收尾；删不到是正常情况）。
    ///
    /// 本机组合在删数据的同一事务里已经删过（`session_data::delete_tree`），这里是幂等的
    /// 空操作；数据在**远端**的组合靠这一步收敛——远端行消失后本机还留着一条代际行，
    /// 那是一条没有对象的行：它会让同名 identity 的重新创建看起来「已有代际」。
    pub(super) async fn delete_execution_state(&self, id: &ThreadId) -> Result<()> {
        sqlx::query("DELETE FROM execution_runs WHERE thread_id = ?")
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(super) async fn require_execution_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExecutionWriteGuard>> {
        if self.read_only {
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
        if self.read_only {
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
