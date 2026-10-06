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

#[path = "gate_control.rs"]
mod control;
#[path = "gate_work.rs"]
mod work;

#[derive(Default)]
struct PendingWrites {
    barrier: Arc<RwLock<()>>,
    uncertain: AtomicBool,
    control: Mutex<Option<peri_acp_types::session_resources::ControlCommand>>,
    work: Mutex<Option<peri_acp_types::session_resources::work::WorkCommand>>,
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
        }
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

    fn check_pending(pending: &PendingWrites, root: &ThreadId) -> SessionResourceResult<()> {
        if pending.uncertain.load(Ordering::Acquire)
            || pending
                .work
                .lock()
                .expect("pending work lock poisoned")
                .is_some()
            || pending
                .control
                .lock()
                .expect("pending control lock poisoned")
                .is_some()
        {
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
        self.check_owned_work(root).await?;
        Ok(WriteScope {
            pending,
            _concurrent: concurrent,
            _exclusive: exclusive,
            settled: false,
        })
    }

    async fn check_owned_work(&self, root: &ThreadId) -> SessionResourceResult<()> {
        match self
            .data
            .load_session_work(&peri_acp_types::session_resources::work::WorkQuery {
                session_id: root.clone(),
                limit: 1,
            })
            .await
        {
            Ok(snapshot) if !snapshot.pending_commands.is_empty() => Err(
                SessionResourceError::persistence_uncertain(Some(root.clone())),
            ),
            Ok(_) => Ok(()),
            Err(error) if matches!(error.kind(), SessionResourceErrorKind::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
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
        if pending
            .work
            .lock()
            .expect("pending work lock poisoned")
            .is_some()
            || pending
                .control
                .lock()
                .expect("pending control lock poisoned")
                .is_some()
        {
            return Ok(PersistenceRecovery::StillBlocked);
        }
        if self.check_owned_work(&root).await.is_err() {
            return Ok(PersistenceRecovery::StillBlocked);
        }
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
        scope.settle(&result);
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
        let saved = self.data().binding_discovery_snapshot(id).await?;
        if matches!(self.home, SessionDataHome::RemoteStore) {
            let saved = saved.ok_or_else(|| {
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
        match saved {
            Some(saved) => {
                let recorded: serde_json::Value = serde_json::from_str(&saved).map_err(|_| {
                    SessionResourceError::new(SessionResourceErrorKind::Workspace(
                        WorkspaceError::InvalidBinding,
                    ))
                })?;
                let observed: serde_json::Value = serde_json::from_str(
                    resolved.discovery_snapshot.as_deref().ok_or_else(|| {
                        SessionResourceError::new(SessionResourceErrorKind::Workspace(
                            WorkspaceError::InvalidBinding,
                        ))
                    })?,
                )
                .map_err(|_| {
                    SessionResourceError::new(SessionResourceErrorKind::Workspace(
                        WorkspaceError::InvalidBinding,
                    ))
                })?;
                if recorded != observed {
                    return Err(SessionResourceError::new(
                        SessionResourceErrorKind::Workspace(
                            WorkspaceError::ExecutionBindingMismatch,
                        ),
                    ));
                }
            }
            None => {
                return Err(SessionResourceError::new(
                    SessionResourceErrorKind::Workspace(WorkspaceError::InvalidBinding),
                ))
            }
        }
        Ok(resolved)
    }
}
