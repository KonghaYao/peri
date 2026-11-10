//! 只保护存储 mutation 生命周期，不登记执行所有者。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::SessionDataHome;
#[cfg(target_os = "emscripten")]
use crate::sessions::failure::execution_failure;
#[cfg(not(target_os = "emscripten"))]
use crate::sessions::sqlite_store::execution_failure;
use peri_acp_types::session_resources::{
    MutationOutcome, PersistenceRecovery, SessionResourceError, SessionResourceErrorKind,
    SessionResourceResult,
};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{ResolvedWorkspace, WorkspaceError};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::sessions::data::SessionDataPort;
use crate::sessions::failure::{read_only_store, unavailable};
use crate::sessions::local_port::LocalExecutionPort;
use crate::sessions::resources::lifecycle::{Lifecycle, LifecycleState};

#[derive(Default)]
struct PendingWrites {
    barrier: Arc<RwLock<()>>,
    uncertain: AtomicBool,
}

/// 进程内认领世代：区分「本次认领」与后续新认领。
///
/// 持久记录无法表达归属：新 run 的正常中间态 `Active + claimed + Working` 与旧认领
/// 写入的值完全相同。世代只在同一门面实例内有效，跨进程/远端写入不适用（见 issue
/// 已知限制），也不替代存储层自身的原子性。
#[derive(Default)]
pub(super) struct ClaimLedger {
    next: u64,
    current: HashMap<ThreadId, u64>,
}

pub(super) struct WriteScope {
    pending: Arc<PendingWrites>,
    _concurrent: Option<OwnedRwLockReadGuard<()>>,
    _exclusive: Option<OwnedRwLockWriteGuard<()>>,
    settled: bool,
}

impl WriteScope {
    pub(super) fn settle<T>(mut self, result: &SessionResourceResult<T>) {
        self.settled = result
            .as_ref()
            .err()
            .is_none_or(|error| error.effect() != MutationOutcome::Unknown);
    }
}

impl Drop for WriteScope {
    fn drop(&mut self) {
        if !self.settled {
            self.pending.uncertain.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone)]
pub(super) struct MutationGate {
    data: Arc<dyn SessionDataPort>,
    local: Arc<dyn LocalExecutionPort>,
    lifecycle: Lifecycle,
    home: SessionDataHome,
    pending: Arc<Mutex<HashMap<ThreadId, Arc<PendingWrites>>>>,
    claims: Arc<Mutex<ClaimLedger>>,
}

impl MutationGate {
    pub(super) fn new(
        data: Arc<dyn SessionDataPort>,
        local: Arc<dyn LocalExecutionPort>,
        lifecycle: Lifecycle,
        home: SessionDataHome,
    ) -> Self {
        Self {
            data,
            local,
            lifecycle,
            home,
            pending: Arc::default(),
            claims: Arc::default(),
        }
    }

    /// 分配本次认领的世代并立即生效：后续认领覆盖它，旧 handle 随即失去归属。
    pub(super) fn issue_claim(&self, child: &ThreadId) -> u64 {
        let mut ledger = self.claims.lock().expect("claim ledger lock poisoned");
        ledger.next += 1;
        let generation = ledger.next;
        ledger.current.insert(child.clone(), generation);
        generation
    }

    /// 校验认领仍属于 `generation`；新认领一旦分配，旧世代立即失效。
    pub(super) fn claim_is_current(&self, child: &ThreadId, generation: u64) -> bool {
        self.claims
            .lock()
            .expect("claim ledger lock poisoned")
            .current
            .get(child)
            .copied()
            == Some(generation)
    }

    /// 回收已完成的认领世代；只有条目仍属于 `generation` 时移除，新认领不受影响。
    ///
    /// 也由 handle 的 `Drop` 调用，因此中毒时静默跳过，避免析构期间再次 panic。
    pub(super) fn release_claim(&self, child: &ThreadId, generation: u64) {
        let Ok(mut ledger) = self.claims.lock() else {
            return;
        };
        if ledger.current.get(child).copied() == Some(generation) {
            ledger.current.remove(child);
        }
    }

    /// 删除会话树后丢弃随树消失的认领世代。
    ///
    /// 删除与认领在同一写屏障上互斥，成功删除后这些 thread 不可能再有有效认领。
    pub(super) fn release_claims(&self, children: &[ThreadId]) {
        if children.is_empty() {
            return;
        }
        let mut ledger = self.claims.lock().expect("claim ledger lock poisoned");
        for child in children {
            ledger.current.remove(child);
        }
    }

    pub(super) fn has_claims(&self) -> bool {
        !self
            .claims
            .lock()
            .expect("claim ledger lock poisoned")
            .current
            .is_empty()
    }

    #[cfg(test)]
    pub(super) fn claim_is_tracked(&self, child: &ThreadId) -> bool {
        self.claims
            .lock()
            .expect("claim ledger lock poisoned")
            .current
            .contains_key(child)
    }

    pub(super) fn data(&self) -> &Arc<dyn SessionDataPort> {
        &self.data
    }

    pub(super) fn local(&self) -> &Arc<dyn LocalExecutionPort> {
        &self.local
    }

    pub(super) fn ensure_open(&self) -> SessionResourceResult<()> {
        if self.lifecycle.state() != LifecycleState::Open {
            return Err(unavailable("session resources are closed"));
        }
        Ok(())
    }

    pub(super) fn ensure_recovery_permitted(&self) -> SessionResourceResult<()> {
        if self.lifecycle.state() == LifecycleState::Closed {
            return Err(unavailable("session resources are closed"));
        }
        Ok(())
    }

    pub(super) fn ensure_recovery_write(&self) -> SessionResourceResult<()> {
        self.ensure_recovery_permitted()?;
        if self.local.is_read_only() {
            return Err(read_only_store());
        }
        Ok(())
    }

    pub(super) fn ensure_session_write(&self) -> SessionResourceResult<()> {
        self.ensure_open()?;
        self.ensure_recovery_write()
    }

    pub(super) fn ensure_registration_write(&self) -> SessionResourceResult<()> {
        self.ensure_open()?;
        if self.local.is_read_only() {
            return Err(WorkspaceError::ReadOnlyStore.into());
        }
        Ok(())
    }

    fn pending_for(&self, root: &ThreadId) -> Arc<PendingWrites> {
        self.pending
            .lock()
            .expect("pending writes lock poisoned")
            .entry(root.clone())
            .or_default()
            .clone()
    }

    /// close/delete 成功后回收 root 的 pending 状态。
    ///
    /// 回收与 `pending_for` 在同一 map 锁内串行化；只有 map 独占该条目
    /// （没有在飞 `WriteScope`）且没有未知结果时才移除，避免回收仍在使用的屏障
    /// 或丢弃需要恢复的证据。
    pub(super) fn release_pending_for(&self, root: &ThreadId) {
        let mut pending = self.pending.lock().expect("pending writes lock poisoned");
        let removable = pending.get(root).is_some_and(|entry| {
            !entry.uncertain.load(Ordering::Acquire) && Arc::strong_count(entry) == 1
        });
        if removable {
            pending.remove(root);
        }
    }

    #[cfg(test)]
    pub(super) fn pending_write_refs(&self) -> Vec<(ThreadId, std::sync::Weak<RwLock<()>>, bool)> {
        self.pending
            .lock()
            .expect("pending writes lock poisoned")
            .iter()
            .map(|(id, pending)| {
                (
                    id.clone(),
                    Arc::downgrade(&pending.barrier),
                    pending.uncertain.load(Ordering::Acquire),
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn mark_pending_uncertain(&self, root: &ThreadId) {
        let mut pending = self.pending.lock().expect("pending writes lock poisoned");
        pending
            .entry(root.clone())
            .or_default()
            .uncertain
            .store(true, Ordering::Release);
    }

    fn check_pending(pending: &PendingWrites, root: &ThreadId) -> SessionResourceResult<()> {
        if pending.uncertain.load(Ordering::Acquire) {
            return Err(SessionResourceError::persistence_uncertain(Some(
                root.clone(),
            )));
        }
        Ok(())
    }

    async fn scope(&self, root: &ThreadId, exclusive: bool) -> SessionResourceResult<WriteScope> {
        let pending = self.pending_for(root);
        let (concurrent, exclusive) = if exclusive {
            (None, Some(pending.barrier.clone().write_owned().await))
        } else {
            (Some(pending.barrier.clone().read_owned().await), None)
        };
        self.ensure_session_write()?;
        Self::check_pending(&pending, root)?;
        Ok(WriteScope {
            pending,
            _concurrent: concurrent,
            _exclusive: exclusive,
            settled: false,
        })
    }

    pub(super) async fn admit(&self, id: &ThreadId) -> SessionResourceResult<WriteScope> {
        self.ensure_session_write()?;
        let root = self.data.session_root(id).await?;
        if self.home == SessionDataHome::RemoteStore {
            self.recheck_binding_of(id, false).await?;
        }
        self.scope(&root, false).await
    }

    pub(super) async fn with_mutation<T, F, Fut>(
        &self,
        id: &ThreadId,
        work: F,
    ) -> SessionResourceResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = SessionResourceResult<T>>,
    {
        let scope = self.admit(id).await?;
        let result = work().await;
        scope.settle(&result);
        result
    }

    pub(super) async fn with_registration<T, F, Fut>(
        &self,
        id: &ThreadId,
        work: F,
    ) -> SessionResourceResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = SessionResourceResult<T>>,
    {
        self.ensure_registration_write()?;
        let scope = self.scope(id, true).await?;
        let result = work().await;
        scope.settle(&result);
        result
    }

    pub(super) async fn with_exclusive<T, F, Fut>(
        &self,
        id: &ThreadId,
        work: F,
    ) -> SessionResourceResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = SessionResourceResult<T>>,
    {
        self.ensure_session_write()?;
        let root = self.data.session_root(id).await?;
        if self.home == SessionDataHome::RemoteStore {
            self.recheck_binding_of(id, false).await?;
        }
        let scope = self.scope(&root, true).await?;
        let result = work().await;
        scope.settle(&result);
        result
    }

    pub(super) async fn recover(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        self.ensure_recovery_permitted()?;
        let root = self.data.session_root(id).await?;
        let pending = self.pending_for(&root);
        let _barrier = peri_time::timeout(super::SETTLE_WAIT, pending.barrier.write())
            .await
            .map_err(|_| {
                SessionResourceError::new(
                    peri_acp_types::session_resources::SessionResourceErrorKind::Timeout,
                )
            })?;
        let result = self.data.recover_persistence(id).await?;
        if matches!(result, PersistenceRecovery::Recovered) && !self.local.is_read_only() {
            pending.uncertain.store(false, Ordering::Release);
        }
        Ok(result)
    }

    pub(super) async fn drain(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.ensure_recovery_permitted()?;
        let root = self.data.session_root(id).await?;
        let pending = self.pending_for(&root);
        let _barrier = peri_time::timeout(super::SETTLE_WAIT, pending.barrier.write())
            .await
            .map_err(|_| {
                SessionResourceError::new(
                    peri_acp_types::session_resources::SessionResourceErrorKind::Timeout,
                )
            })?;
        Self::check_pending(&pending, &root)?;
        self.data.drain(id).await
    }

    pub(super) async fn finish_close(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.ensure_recovery_write()?;
        let root = self.data.session_root(id).await?;
        let pending = self.pending_for(&root);
        let barrier = peri_time::timeout(super::SETTLE_WAIT, pending.barrier.clone().write_owned())
            .await
            .map_err(|_| {
                SessionResourceError::new(
                    peri_acp_types::session_resources::SessionResourceErrorKind::Timeout,
                )
            })?;
        Self::check_pending(&pending, &root)?;
        self.data.drain(id).await?;
        let scope = WriteScope {
            pending,
            _concurrent: None,
            _exclusive: Some(barrier),
            settled: false,
        };
        let result = self.data.finish_close(id).await;
        let finished = result.is_ok();
        scope.settle(&result);
        if finished && root == *id {
            self.release_pending_for(&root);
        }
        result
    }

    pub(super) async fn drain_all(&self) -> SessionResourceResult<()> {
        let pending: Vec<_> = self
            .pending
            .lock()
            .expect("pending writes lock poisoned")
            .iter()
            .map(|(root, pending)| (root.clone(), pending.clone()))
            .collect();
        for (root, pending) in pending {
            let _barrier = peri_time::timeout(super::SETTLE_WAIT, pending.barrier.write())
                .await
                .map_err(|_| {
                    SessionResourceError::new(
                        peri_acp_types::session_resources::SessionResourceErrorKind::Timeout,
                    )
                })?;
            Self::check_pending(&pending, &root)?;
            self.data.drain(&root).await?;
        }
        Ok(())
    }
    pub(super) async fn recheck_binding_of(
        &self,
        id: &ThreadId,
        full: bool,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        let binding = self.data().binding_of(id).await?;
        let binding = binding.as_ref().ok_or_else(|| {
            SessionResourceError::new(SessionResourceErrorKind::Workspace(
                WorkspaceError::BindingMissing,
            ))
        })?;
        let owner = self
            .data()
            .workspace_id_of(id)
            .await?
            .ok_or_else(|| SessionResourceError::new(SessionResourceErrorKind::NotFound))?;
        if matches!(self.home, SessionDataHome::RemoteStore) {
            let saved = self
                .data()
                .binding_discovery_snapshot(id)
                .await?
                .ok_or_else(|| {
                    SessionResourceError::new(SessionResourceErrorKind::Workspace(
                        WorkspaceError::InvalidBinding,
                    ))
                })?;
            let resolved = self
                .local()
                .validate_saved_binding(binding, &saved, owner, full)
                .await
                .map_err(execution_failure)?;
            if owner != resolved.workspace_id {
                return Err(SessionResourceError::new(
                    SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionBindingMismatch),
                ));
            }
            return Ok(resolved);
        }
        // 本地组合的快照只作诊断：文件对象身份会随容器重启变化，不参与绑定复核。
        let resolved = self
            .local()
            .validate_binding_value(binding, full)
            .await
            .map_err(execution_failure)?;
        if owner != resolved.workspace_id {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionBindingMismatch),
            ));
        }
        Ok(resolved)
    }
}
