//! SQLite adapter for storage-independent execution leases.

use super::database::SqliteSessionDatabase;
pub(in crate::sessions) use crate::sessions::execution::{
    ExclusiveExecutionGuard, ExecutionLease, ExecutionWriteGuard,
};
use crate::sessions::local_port::SessionFacts;
use anyhow::Result;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{SessionExecutionLease, WorkspaceError};
use std::sync::Arc;

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
            if !lease.can_replace() {
                return Err(WorkspaceError::ExecutionLeaseRequired.into());
            }
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
        Ok(Some(lease.write_guard().await?))
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
        Ok(Some(lease.exclusive_guard().await?))
    }
}
