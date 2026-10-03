//! Storage-independent in-process execution leases and mutation guards.

use anyhow::Result;
use async_trait::async_trait;
use peri_acp_types::{
    session_resources::{MutationOutcome, SessionResourceError, SessionResourceResult},
    thread::ThreadId,
    workspace::{ExecutionOwnerToken, PriorExecutionOwner, SessionExecutionLease, WorkspaceError},
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
    owner_token: std::sync::RwLock<Option<ExecutionOwnerToken>>,
    prior_unreleased: std::sync::RwLock<Option<PriorExecutionOwner>>,
}

impl ExecutionLease {
    pub(in crate::sessions) fn install_owner_token(&self, token: ExecutionOwnerToken) {
        *self.owner_token.write().expect("execution owner token lock poisoned") = Some(token);
    }

    pub(in crate::sessions) fn install_prior_unreleased(&self, prior: Option<PriorExecutionOwner>) {
        *self.prior_unreleased.write().expect("execution prior owner lock poisoned") = prior;
    }

    pub(in crate::sessions) fn new(thread_id: ThreadId) -> Self {
        Self {
            thread_id,
            active: AtomicBool::new(true),
            mutation_gate: Arc::new(tokio::sync::RwLock::new(())),
            mutation_uncertain: AtomicBool::new(false),
            owner_token: std::sync::RwLock::new(None),
            prior_unreleased: std::sync::RwLock::new(None),
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

    pub(in crate::sessions) fn can_replace(&self) -> bool {
        self.mutation_gate.try_write().is_ok()
    }

    pub(in crate::sessions) async fn write_guard(self: &Arc<Self>) -> Result<ExecutionWriteGuard> {
        let gate = self.mutation_gate.clone().read_owned().await;
        if !self.is_active() {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        if self.is_uncertain() {
            anyhow::bail!("session persistence outcome is uncertain");
        }
        Ok(ExecutionWriteGuard {
            lease: Arc::clone(self),
            _gate: gate,
            completed: false,
        })
    }

    pub(in crate::sessions) async fn exclusive_guard(
        self: &Arc<Self>,
    ) -> Result<ExclusiveExecutionGuard> {
        let gate = self.mutation_gate.clone().write_owned().await;
        if !self.is_active() {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        if self.is_uncertain() {
            anyhow::bail!("session persistence outcome is uncertain");
        }
        Ok(ExclusiveExecutionGuard {
            lease: Arc::clone(self),
            _gate: gate,
            completed: false,
        })
    }
}

/// The guard retains the lease and admission lock until a mutation settles.
/// Dropping an unsettled mutation records an uncertain effect.
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
/// 并发（存储 adapter 负责自己的并发控制），写侧门禁用来把「检查 + 写入」做成一段不可插入的
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
/// 本地 SQLite adapter 的事务在 `commit()` 失败后由连接回滚，但「回滚是否真的完成」不由
/// 调用方观察得到；这里把不可证明的那一小段标出来，让写入准入保持未决，而不是用
/// `Err` 冒充「没生效」。
#[derive(Default)]
pub(in crate::sessions) struct TransactionEffect {
    committing: bool,
    committed: bool,
}

impl TransactionEffect {
    pub(in crate::sessions) fn new() -> Self {
        Self::default()
    }

    /// 即将调用 `commit()`：此后失败不再能证明未生效。
    pub(in crate::sessions) fn enter_commit(&mut self) {
        self.committing = true;
    }

    /// `commit()` 已成功返回。
    pub(in crate::sessions) fn commit_succeeded(&mut self) {
        self.committing = false;
        self.committed = true;
    }

    /// 按效果结清准入：可证明「未生效」或「已生效」时才 `finish`；其余情况释放 guard
    /// 由 `Drop` 留下未决证据。
    pub(in crate::sessions) fn settle(&self, guard: Option<ExecutionWriteGuard>) {
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

    fn owner_token(&self) -> Option<ExecutionOwnerToken> {
        self.owner_token.read().ok()?.clone()
    }

    fn prior_unreleased_generation(&self) -> Option<PriorExecutionOwner> {
        self.prior_unreleased.read().expect("execution prior owner lock poisoned").clone()
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
            return Err(SessionResourceError::conflict(
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

/// Check that a trait object refers to the exact active lease allocation.
pub(in crate::sessions) fn same_lease(
    owned: &Arc<ExecutionLease>,
    lease: &Arc<dyn SessionExecutionLease>,
) -> bool {
    std::ptr::eq(
        Arc::as_ptr(owned) as *const (),
        Arc::as_ptr(lease) as *const (),
    )
}
