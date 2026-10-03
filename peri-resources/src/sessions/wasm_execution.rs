//! Virtual workspace authority and process-local execution leases for WASM.
//!
//! The host supplies one stable workspace root and machine identity at startup.
//! No filesystem existence is inferred here: builtin filesystem tools are absent
//! from this target, while canonical session data stays in the Turso adapter.
use super::{
    execution::{same_lease, ExclusiveExecutionGuard, ExecutionLease, ExecutionWriteGuard},
    local_port::{LocalExecutionPort, RevokeEffect, SessionFacts},
};
use anyhow::{ensure, Result};
use async_trait::async_trait;
use peri_acp_types::{
    session_resources::{FrozenSnapshotBytes, NewSession, NewSessionDraft, SessionResourceResult},
    thread::ThreadId,
    workspace::{
        ProjectId, ResolvedWorkspace, SessionBinding, SessionExecutionLease, WorkspaceError,
        WorkspaceId,
    },
};
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

pub(in crate::sessions) struct WasmExecution {
    workspace: ResolvedWorkspace,
    read_only: bool,
    leases: Mutex<HashMap<ThreadId, Arc<ExecutionLease>>>,
}

impl WasmExecution {
    pub(in crate::sessions) fn new(root: PathBuf, machine_id: &str) -> Result<Self> {
        ensure!(root.is_absolute(), "WASM workspace root must be absolute");
        let root = normalize(&root)?;
        let project_id = stable_id("project", machine_id, &root)?;
        let workspace_id = stable_id("workspace", machine_id, &root)?;
        Ok(Self {
            workspace: ResolvedWorkspace {
                project_id: ProjectId::from_str(&project_id)?,
                workspace_id: WorkspaceId::from_str(&workspace_id)?,
                execution_registration_id: WorkspaceId::from_str(&workspace_id)?,
                cwd: root.clone(),
                root: root.clone(),
                relative_cwd: PathBuf::new(),
                discovery_snapshot: Some(serde_json::json!({"root": root}).to_string()),
            },
            read_only: false,
            leases: Mutex::new(HashMap::new()),
        })
    }

    pub(in crate::sessions) fn read_only() -> Self {
        let mut instance = Self::new(PathBuf::from("/"), "read-only").expect("root is valid");
        instance.read_only = true;
        instance
    }

    fn registered(&self, id: &ThreadId) -> Result<Option<Arc<ExecutionLease>>> {
        Ok(self
            .leases
            .lock()
            .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?
            .get(id)
            .cloned())
    }

    fn register(&self, id: &ThreadId) -> Result<Arc<ExecutionLease>> {
        if self.read_only {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?;
        if let Some(lease) = leases.get(id) {
            ensure!(
                !lease.is_uncertain(),
                "session persistence outcome is uncertain"
            );
            if lease.is_active() {
                return Ok(lease.clone());
            }
            let _closed = lease
                .mutation_gate
                .try_write()
                .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?;
        }
        let lease = Arc::new(ExecutionLease::new(id.clone()));
        leases.insert(id.clone(), lease.clone());
        Ok(lease)
    }

    fn require_owner(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<Arc<ExecutionLease>> {
        let owned = self
            .registered(id)
            .map_err(super::failure::execution_failure)?
            .ok_or_else(super::failure::lease_required)?;
        if !same_lease(&owned, lease) || !owned.is_active() || owned.is_uncertain() {
            return Err(super::failure::lease_required());
        }
        Ok(owned)
    }
}

fn stable_id(kind: &str, machine_id: &str, root: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(b"peri-wasm-workspace-v1\0");
    hash.update(kind.as_bytes());
    hash.update(b"\0");
    hash.update(machine_id.as_bytes());
    hash.update(b"\0");
    hash.update(root.to_str().ok_or(WorkspaceError::Unavailable)?.as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Uuid::from_bytes(bytes).to_string())
}

fn normalize(path: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::RootDir => out.push(component),
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                ensure!(out.pop(), "WASM workspace path escapes root");
            }
            std::path::Component::Prefix(_) => return Err(WorkspaceError::Unavailable.into()),
        }
    }
    Ok(out)
}

#[async_trait]
impl LocalExecutionPort for WasmExecution {
    fn is_read_only(&self) -> bool {
        self.read_only
    }

    async fn resolve_workspace(&self, cwd: &Path) -> Result<ResolvedWorkspace> {
        if self.read_only {
            return Err(WorkspaceError::ReadOnlyStore.into());
        }
        if normalize(cwd)? != self.workspace.cwd {
            return Err(WorkspaceError::Unavailable.into());
        }
        Ok(self.workspace.clone())
    }

    async fn validate_binding_value(
        &self,
        binding: &SessionBinding,
        _full: bool,
    ) -> Result<ResolvedWorkspace> {
        if binding != &SessionBinding::from_workspace(&self.workspace) {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        Ok(self.workspace.clone())
    }

    async fn validate_saved_binding(
        &self,
        binding: &SessionBinding,
        snapshot: &str,
        owner: WorkspaceId,
        full: bool,
    ) -> Result<ResolvedWorkspace> {
        if owner != self.workspace.workspace_id
            || Some(snapshot) != self.workspace.discovery_snapshot.as_deref()
        {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        self.validate_binding_value(binding, full).await
    }

    async fn legacy_confirmed(&self, _id: &ThreadId) -> Result<bool> {
        Ok(false)
    }

    async fn owner_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        if let Some(lease) = self.registered(id)? {
            return Ok(Some(lease));
        }
        if facts.root != *id {
            return self.registered(&facts.root);
        }
        Ok(None)
    }

    async fn live_owner(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        self.owner_lease(id, facts).await
    }

    fn live_leases(&self) -> Vec<Arc<ExecutionLease>> {
        self.leases
            .lock()
            .map(|leases| leases.values().cloned().collect())
            .unwrap_or_default()
    }

    async fn write_guard(
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
        let gate = lease.mutation_gate.clone().read_owned().await;
        if !lease.active.load(Ordering::Acquire) {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        ensure!(
            !lease.is_uncertain(),
            "session persistence outcome is uncertain"
        );
        Ok(Some(ExecutionWriteGuard {
            lease,
            _gate: gate,
            completed: false,
        }))
    }

    async fn exclusive_guard(
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
        let gate = lease.mutation_gate.clone().write_owned().await;
        if !lease.active.load(Ordering::Acquire) {
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
        }
        ensure!(
            !lease.is_uncertain(),
            "session persistence outcome is uncertain"
        );
        Ok(Some(ExclusiveExecutionGuard {
            lease,
            _gate: gate,
            completed: false,
        }))
    }

    async fn acquire_lease(
        &self,
        id: &ThreadId,
        _facts: &SessionFacts,
    ) -> Result<Arc<dyn SessionExecutionLease>> {
        Ok(self.register(id)?)
    }

    async fn create_session(
        &self,
        _input: &NewSession,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        Err(super::failure::lease_required()) // RemoteStore writes canonical data before admission.
    }

    async fn begin_initialization(
        &self,
        _draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        Err(super::failure::lease_required()) // RemoteStore writes the draft before admission.
    }

    async fn commit_frozen(
        &self,
        _id: &ThreadId,
        _lease: &Arc<dyn SessionExecutionLease>,
        _frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        Err(super::failure::lease_required()) // RemoteStore commits frozen through its data port.
    }

    async fn verify_initialization_owner(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()> {
        self.require_owner(id, lease).map(|_| ())
    }

    async fn discard_incomplete_initialization(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        Err(super::failure::lease_required()) // RemoteStore uses its data port for this operation.
    }

    async fn admit_existing(
        &self,
        id: &ThreadId,
        binding: &SessionBinding,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.validate_binding_value(binding, true)
            .await
            .map_err(super::failure::execution_failure)?;
        self.register(id)
            .map(|lease| lease as Arc<dyn SessionExecutionLease>)
            .map_err(super::failure::execution_failure)
    }

    async fn abandon_initialization(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
        revoke: RevokeEffect<'_>,
    ) -> SessionResourceResult<()> {
        let owned = self.require_owner(id, lease)?;
        owned.abandon_ownership(|| revoke).await
    }

    async fn dispose_execution(&self, id: &ThreadId) -> SessionResourceResult<()> {
        if let Some(lease) = self
            .registered(id)
            .map_err(super::failure::execution_failure)?
        {
            lease.dispose_ownership().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACHINE: &str = "b8109976-c984-4363-940e-1e81f14da579";

    #[tokio::test]
    async fn virtual_workspace_identity_survives_reopen() {
        let first = WasmExecution::new(PathBuf::from("/workspace/project"), MACHINE).unwrap();
        let reopened = WasmExecution::new(PathBuf::from("/workspace/project"), MACHINE).unwrap();
        let workspace = first
            .resolve_workspace(Path::new("/workspace/project"))
            .await
            .unwrap();
        assert_eq!(
            workspace,
            reopened
                .resolve_workspace(Path::new("/workspace/project"))
                .await
                .unwrap()
        );
        let binding = SessionBinding::from_workspace(&workspace);
        assert_eq!(
            reopened
                .validate_binding_value(&binding, true)
                .await
                .unwrap(),
            workspace
        );
        assert!(reopened
            .resolve_workspace(Path::new("/workspace/other"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn lease_drains_and_reopens_after_clean_completion() {
        let local = WasmExecution::new(PathBuf::from("/workspace/project"), MACHINE).unwrap();
        let id = "wasm-session".to_owned();
        let facts = SessionFacts { root: id.clone() };
        let first = local.acquire_lease(&id, &facts).await.unwrap();
        let guard = local.write_guard(&id, &facts).await.unwrap().unwrap();
        guard.finish();
        first.mark_clean().await.unwrap();
        let second = local.acquire_lease(&id, &facts).await.unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(second.thread_id() == &id);
    }

    #[tokio::test]
    async fn history_only_mode_rejects_execution_ownership() {
        let local = WasmExecution::read_only();
        let id = "history-only-session".to_owned();
        let facts = SessionFacts { root: id.clone() };
        assert!(local.is_read_only());
        assert!(local.acquire_lease(&id, &facts).await.is_err());
        assert!(local.write_guard(&id, &facts).await.is_err());
    }
}
