//! Workspace discovery and saved-binding validation for a remote session store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{
    ProjectId, ResolvedWorkspace, SessionBinding, WorkspaceError, WorkspaceId,
    SESSION_BINDING_VERSION,
};
use sha2::{Digest, Sha256};

#[cfg(not(target_os = "emscripten"))]
use crate::sessions::discovery::{self, Discovery};
use crate::sessions::local_port::LocalExecutionPort;

use super::environment::RemoteWorkspaceEnvironment;
use super::session_data::RemoteSessionData;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct VirtualSnapshot {
    kind: String,
    root: PathBuf,
}

pub(super) struct RemoteExecution {
    data: Arc<RemoteSessionData>,
    read_only: bool,
    observations: Mutex<HashMap<(WorkspaceId, PathBuf), ResolvedWorkspace>>,
    environment: RemoteWorkspaceEnvironment,
}

impl RemoteExecution {
    pub(super) fn new(data: Arc<RemoteSessionData>, read_only: bool) -> Self {
        Self::new_in_environment(data, read_only, RemoteWorkspaceEnvironment::Native)
    }

    pub(super) fn new_in_environment(
        data: Arc<RemoteSessionData>,
        read_only: bool,
        environment: RemoteWorkspaceEnvironment,
    ) -> Self {
        Self {
            data,
            read_only,
            observations: Mutex::new(HashMap::new()),
            environment,
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
        if let RemoteWorkspaceEnvironment::Virtual { machine_id, root } = &self.environment {
            let saved: VirtualSnapshot =
                serde_json::from_str(snapshot).map_err(|_| WorkspaceError::InvalidBinding)?;
            if saved.kind != "virtual-v1" || saved.root != *root {
                return Err(WorkspaceError::NeedsRelink.into());
            }
            let cwd = root.join(&binding.cwd_relative_to_workspace);
            let path = root.to_str().ok_or(WorkspaceError::InvalidBinding)?;
            let recorded = self.data.workspace_for_path(machine_id, path).await?;
            if recorded != Some(owner)
                && (require_record
                    || recorded.is_some()
                    || Self::stable_id("workspace", machine_id, root)?.parse::<WorkspaceId>()?
                        != owner)
            {
                return Err(WorkspaceError::ExecutionBindingMismatch.into());
            }
            let expected_project: ProjectId =
                Self::stable_id("project", machine_id, root)?.parse()?;
            if binding.project_id != expected_project {
                return Err(WorkspaceError::ExecutionBindingMismatch.into());
            }
            return Ok(ResolvedWorkspace {
                project_id: expected_project,
                workspace_id: owner,
                execution_registration_id: binding.workspace_id,
                cwd,
                root: root.clone(),
                relative_cwd: binding.cwd_relative_to_workspace.clone(),
                discovery_snapshot: Some(snapshot.to_owned()),
            });
        }
        #[cfg(not(target_os = "emscripten"))]
        {
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
                    || Self::stable_id("workspace", machine, &saved.root)?
                        .parse::<WorkspaceId>()?
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
        #[cfg(target_os = "emscripten")]
        {
            Err(WorkspaceError::Unavailable.into())
        }
    }
}

#[async_trait]
impl LocalExecutionPort for RemoteExecution {
    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn machine_id(&self) -> Result<String> {
        self.environment.machine_id()
    }

    async fn directory_available(&self, cwd: &Path) -> bool {
        match &self.environment {
            RemoteWorkspaceEnvironment::Native => {
                #[cfg(not(target_os = "emscripten"))]
                {
                    tokio::fs::metadata(cwd)
                        .await
                        .map(|meta| meta.is_dir())
                        .unwrap_or(false)
                }
                #[cfg(target_os = "emscripten")]
                {
                    false
                }
            }
            RemoteWorkspaceEnvironment::Virtual { .. } => self.environment.contains(cwd),
        }
    }

    async fn resolve_workspace(&self, cwd: &Path) -> Result<ResolvedWorkspace> {
        if self.read_only {
            return Err(WorkspaceError::ReadOnlyStore.into());
        }
        if let RemoteWorkspaceEnvironment::Virtual { machine_id, root } = &self.environment {
            if !self.environment.contains(cwd) {
                return Err(WorkspaceError::Unavailable.into());
            }
            let relative_cwd = cwd.strip_prefix(root)?.to_path_buf();
            let path = root.to_str().ok_or(WorkspaceError::InvalidBinding)?;
            let workspace_id = match self.data.workspace_for_path(machine_id, path).await? {
                Some(id) => id,
                None => Self::stable_id("workspace", machine_id, root)?.parse()?,
            };
            let project_id = Self::stable_id("project", machine_id, root)?.parse()?;
            let snapshot = serde_json::to_string(&VirtualSnapshot {
                kind: "virtual-v1".to_owned(),
                root: root.clone(),
            })?;
            let resolved = ResolvedWorkspace {
                project_id,
                workspace_id,
                execution_registration_id: workspace_id,
                cwd: cwd.to_path_buf(),
                root: root.clone(),
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
            return Ok(resolved);
        }
        #[cfg(not(target_os = "emscripten"))]
        {
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
        #[cfg(target_os = "emscripten")]
        {
            Err(WorkspaceError::Unavailable.into())
        }
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
}
