//! In-process execution admission for a remote-only session store.
//! Filesystem observations are local; ownership and immutable evidence are remote.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use peri_acp_types::session_resources::{
    NewSession, NewSessionDraft, SessionResourceError, SessionResourceErrorKind,
    SessionResourceResult,
};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{
    ProjectId, ResolvedWorkspace, SessionBinding, SessionExecutionLease, WorkspaceError,
    WorkspaceId, SESSION_BINDING_VERSION,
};
use sha2::{Digest, Sha256};

use crate::sessions::data::SessionDataPort;
use crate::sessions::discovery::{self, Discovery};
use crate::sessions::execution::{
    same_lease, ExclusiveExecutionGuard, ExecutionLease, ExecutionWriteGuard,
};
use crate::sessions::local_port::{LocalExecutionPort, RevokeEffect, SessionFacts};

use super::session_data::RemoteSessionData;

fn lease_required() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Workspace(
        WorkspaceError::ExecutionLeaseRequired,
    ))
}

fn execution_failure(error: anyhow::Error) -> SessionResourceError {
    if let Some(workspace) = error.downcast_ref::<WorkspaceError>() {
        SessionResourceError::new(SessionResourceErrorKind::Workspace(workspace.clone()))
    } else {
        SessionResourceError::new(SessionResourceErrorKind::Unavailable {
            detail: "remote execution state is unavailable".to_owned(),
        })
    }
}

pub(super) struct RemoteExecution {
    data: Arc<RemoteSessionData>,
    read_only: bool,
    leases: Mutex<HashMap<ThreadId, Arc<ExecutionLease>>>,
    observations: Mutex<HashMap<(WorkspaceId, PathBuf), ResolvedWorkspace>>,
}

impl RemoteExecution {
    pub(super) fn new(data: Arc<RemoteSessionData>, read_only: bool) -> Self {
        Self {
            data,
            read_only,
            leases: Mutex::new(HashMap::new()),
            observations: Mutex::new(HashMap::new()),
        }
    }

    fn stable_id(kind: &str, machine: &str, path: &Path) -> Result<String> {
        let path = path.to_str().ok_or(WorkspaceError::InvalidBinding)?;
        let mut digest = Sha256::new();
        digest.update(b"peri.remote.workspace.v2\0");
        digest.update(kind.as_bytes());
        digest.update([0]);
        digest.update(machine.as_bytes());
        digest.update([0]);
        digest.update(path.as_bytes());
        let mut bytes: [u8; 16] = digest.finalize()[..16].try_into()?;
        bytes[6] = (bytes[6] & 0x0f) | 0x80; // RFC 9562 UUIDv8 (custom SHA-256 namespace).
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Ok(uuid::Uuid::from_bytes(bytes).to_string())
    }

    fn registered(&self, id: &ThreadId) -> Result<Option<Arc<ExecutionLease>>> {
        Ok(self
            .leases
            .lock()
            .map_err(|_| WorkspaceError::ExecutionLeaseRequired)?
            .get(id)
            .cloned())
    }

    fn owner(&self, id: &ThreadId, facts: &SessionFacts) -> Result<Option<Arc<ExecutionLease>>> {
        match self.registered(id)? {
            Some(lease) => Ok(Some(lease)),
            None if facts.root != *id => self.registered(&facts.root),
            None => Ok(None),
        }
    }

    fn register(&self, id: &ThreadId) -> Result<Arc<ExecutionLease>> {
        if self.read_only {
            return Err(WorkspaceError::ReadOnlyStore.into());
        }
        let mut leases = self
            .leases
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

    async fn check_saved_write_evidence(&self, id: &ThreadId, facts: &SessionFacts) -> Result<()> {
        let own_binding = self.data.binding_of(id).await?;
        let evidence_id = if own_binding.is_some() {
            id
        } else {
            &facts.root
        };
        let binding = match own_binding {
            Some(binding) => binding,
            None => self
                .data
                .binding_of(&facts.root)
                .await?
                .ok_or(WorkspaceError::BindingMissing)?,
        };
        let snapshot = self
            .data
            .binding_discovery_snapshot(evidence_id)
            .await?
            .ok_or(WorkspaceError::BindingMissing)?;
        let owner = self
            .data
            .workspace_id_of(evidence_id)
            .await?
            .ok_or(WorkspaceError::InvalidBinding)?;
        self.observe_binding(&binding, &snapshot, owner, true)
            .await?;
        Ok(())
    }

    async fn observe_binding(
        &self,
        binding: &SessionBinding,
        snapshot: &str,
        owner: WorkspaceId,
        require_record: bool,
    ) -> Result<ResolvedWorkspace> {
        if binding.schema_version != SESSION_BINDING_VERSION || binding.revision != 1 {
            return Err(WorkspaceError::InvalidBinding.into());
        }
        if binding
            .cwd_relative_to_workspace
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(WorkspaceError::InvalidBinding.into());
        }
        let saved: Discovery =
            serde_json::from_str(snapshot).map_err(|_| WorkspaceError::InvalidBinding)?;
        let cwd = if binding.cwd_relative_to_workspace.as_os_str().is_empty() {
            saved.root.clone()
        } else {
            saved.root.join(&binding.cwd_relative_to_workspace)
        };
        let (canonical_cwd, observed) = discovery::observe(&cwd).await?;
        if canonical_cwd != cwd || observed.discovery != saved {
            return Err(WorkspaceError::NeedsRelink.into());
        }
        let machine = crate::sessions::machine::current()?;
        let path = saved.root.to_str().ok_or(WorkspaceError::InvalidBinding)?;
        let recorded = self.data.workspace_for_path(machine, path).await?;
        if recorded != Some(owner)
            && (require_record
                || recorded.is_some()
                || Self::stable_id("workspace", machine, &saved.root)?.parse::<WorkspaceId>()?
                    != owner)
        {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        Ok(ResolvedWorkspace {
            project_id: binding.project_id,
            workspace_id: owner,
            execution_registration_id: binding.workspace_id,
            cwd: canonical_cwd,
            root: saved.root,
            relative_cwd: binding.cwd_relative_to_workspace.clone(),
            discovery_snapshot: Some(snapshot.to_owned()),
        })
    }
}

#[async_trait]
impl LocalExecutionPort for RemoteExecution {
    fn is_read_only(&self) -> bool {
        self.read_only
    }

    async fn resolve_workspace(&self, cwd: &Path) -> Result<ResolvedWorkspace> {
        if self.read_only {
            return Err(WorkspaceError::ReadOnlyStore.into());
        }
        let (cwd, observation) = discovery::observe(cwd).await?;
        let discovery = observation.discovery;
        let machine = crate::sessions::machine::current()?;
        let path = discovery
            .root
            .to_str()
            .ok_or(WorkspaceError::InvalidBinding)?;
        let workspace_id = match self.data.workspace_for_path(machine, path).await? {
            Some(id) => id,
            None => Self::stable_id("workspace", machine, &discovery.root)?.parse()?,
        };
        let project_id: ProjectId =
            Self::stable_id("project", machine, discovery.project_locator())?.parse()?;
        let snapshot = serde_json::to_string(&discovery)?;
        discovery.reassert_key_objects(&cwd).await?;
        let relative_cwd = cwd.strip_prefix(&discovery.root)?.to_path_buf();
        let resolved = ResolvedWorkspace {
            project_id,
            workspace_id,
            execution_registration_id: workspace_id,
            cwd,
            root: discovery.root,
            relative_cwd,
            discovery_snapshot: Some(snapshot),
        };
        self.observations
            .lock()
            .map_err(|_| WorkspaceError::Unavailable)?
            .insert(
                (workspace_id, resolved.relative_cwd.clone()),
                resolved.clone(),
            );
        Ok(resolved)
    }

    async fn validate_binding_value(
        &self,
        binding: &SessionBinding,
        _full: bool,
    ) -> Result<ResolvedWorkspace> {
        let observed = self
            .observations
            .lock()
            .map_err(|_| WorkspaceError::Unavailable)?
            .get(&(
                binding.workspace_id,
                binding.cwd_relative_to_workspace.clone(),
            ))
            .cloned()
            .ok_or(WorkspaceError::InvalidBinding)?;
        if binding.project_id != observed.project_id
            || binding.cwd_relative_to_workspace != observed.relative_cwd
        {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        let snapshot = observed
            .discovery_snapshot
            .as_deref()
            .ok_or(WorkspaceError::InvalidBinding)?;
        self.observe_binding(binding, snapshot, observed.workspace_id, false)
            .await
    }

    async fn validate_saved_binding(
        &self,
        binding: &SessionBinding,
        snapshot: &str,
        owner: WorkspaceId,
        _full: bool,
    ) -> Result<ResolvedWorkspace> {
        let resolved = self.observe_binding(binding, snapshot, owner, true).await?;
        self.observations
            .lock()
            .map_err(|_| WorkspaceError::Unavailable)?
            .insert(
                (
                    binding.workspace_id,
                    binding.cwd_relative_to_workspace.clone(),
                ),
                resolved.clone(),
            );
        Ok(resolved)
    }

    async fn legacy_confirmed(&self, _id: &ThreadId) -> Result<bool> {
        Ok(false)
    }
    async fn owner_lease(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        self.owner(id, facts)
    }
    async fn live_owner(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<Arc<ExecutionLease>>> {
        self.owner(id, facts)
    }
    fn live_leases(&self) -> Vec<Arc<ExecutionLease>> {
        self.leases
            .lock()
            .map(|map| map.values().cloned().collect())
            .unwrap_or_default()
    }
    async fn write_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExecutionWriteGuard>> {
        if self.read_only {
            return Err(WorkspaceError::ReadOnlyStore.into());
        }
        match self.owner(id, facts)? {
            Some(lease) => Ok(Some(lease.write_guard().await?)),
            None => {
                self.check_saved_write_evidence(id, facts).await?;
                Ok(None)
            }
        }
    }
    async fn exclusive_guard(
        &self,
        id: &ThreadId,
        facts: &SessionFacts,
    ) -> Result<Option<ExclusiveExecutionGuard>> {
        if self.read_only {
            return Err(WorkspaceError::ReadOnlyStore.into());
        }
        match self.owner(id, facts)? {
            Some(lease) => Ok(Some(lease.exclusive_guard().await?)),
            None => Ok(None),
        }
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
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Workspace(WorkspaceError::Unavailable),
        ))
    }
    async fn begin_initialization(
        &self,
        _draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Workspace(WorkspaceError::Unavailable),
        ))
    }
    async fn commit_frozen(
        &self,
        _id: &ThreadId,
        _lease: &Arc<dyn SessionExecutionLease>,
        _frozen: &peri_acp_types::session_resources::FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Workspace(WorkspaceError::Unavailable),
        ))
    }
    async fn verify_initialization_owner(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
    ) -> SessionResourceResult<()> {
        let owned = self
            .registered(id)
            .map_err(execution_failure)?
            .ok_or_else(lease_required)?;
        if !same_lease(&owned, lease) || !owned.is_active() || owned.is_uncertain() {
            return Err(lease_required());
        }
        Ok(())
    }
    async fn discard_incomplete_initialization(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Workspace(WorkspaceError::Unavailable),
        ))
    }
    async fn admit_existing(
        &self,
        id: &ThreadId,
        _binding: &SessionBinding,
    ) -> SessionResourceResult<Arc<dyn SessionExecutionLease>> {
        self.register(id)
            .map(|lease| lease as Arc<dyn SessionExecutionLease>)
            .map_err(execution_failure)
    }
    async fn abandon_initialization(
        &self,
        id: &ThreadId,
        lease: &Arc<dyn SessionExecutionLease>,
        revoke: RevokeEffect<'_>,
    ) -> SessionResourceResult<()> {
        self.verify_initialization_owner(id, lease).await?;
        let owned = self
            .registered(id)
            .map_err(execution_failure)?
            .ok_or_else(lease_required)?;
        owned.abandon_ownership(|| revoke).await
    }
    async fn dispose_execution(&self, id: &ThreadId) -> SessionResourceResult<()> {
        if let Some(lease) = self.registered(id).map_err(execution_failure)? {
            lease.dispose_ownership().await;
        }
        Ok(())
    }
}
