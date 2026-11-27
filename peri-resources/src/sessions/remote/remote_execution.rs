//! Workspace discovery and saved-binding validation for a remote session store.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::ProjectId;
use peri_acp_types::workspace::ResolvedWorkspace;
use peri_acp_types::workspace::SessionBinding;
use peri_acp_types::workspace::WorkspaceError;
use peri_acp_types::workspace::WorkspaceId;
use peri_acp_types::workspace::SESSION_BINDING_VERSION;
use sha2::{Digest, Sha256};

use crate::sessions::discovery::{self, Discovery};
use crate::sessions::local_port::LocalExecutionPort;

use super::environment::RemoteWorkspaceEnvironment;
use super::session_data::RemoteSessionData;

/// 进程内工作区观测的保留上限。
///
/// 观测只是准入前的本机复核证据，不是持久事实：未命中时调用方按持久化快照重新观测。
/// 因此缓存有界不会影响恢复，只影响同一进程内重复准入的命中率。
const MAX_WORKSPACE_OBSERVATIONS: usize = 256;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct VirtualSnapshot {
    kind: String,
    root: PathBuf,
}

/// 有界的 workspace/cwd 观测表：命中刷新顺序，超限淘汰最久未使用的一条。
#[derive(Default)]
struct ObservationCache {
    entries: HashMap<(WorkspaceId, PathBuf), ResolvedWorkspace>,
    /// 插入/命中顺序，队首是下一次淘汰候选；可能残留已移除的键。
    order: VecDeque<(WorkspaceId, PathBuf)>,
}

impl ObservationCache {
    fn record(&mut self, key: (WorkspaceId, PathBuf), resolved: ResolvedWorkspace) {
        if self.entries.contains_key(&key) {
            self.touch(&key);
            self.entries.insert(key, resolved);
            return;
        }
        while self.entries.len() >= MAX_WORKSPACE_OBSERVATIONS {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, resolved);
    }

    fn get(&mut self, key: &(WorkspaceId, PathBuf)) -> Option<ResolvedWorkspace> {
        let resolved = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(resolved)
    }

    fn touch(&mut self, key: &(WorkspaceId, PathBuf)) {
        if let Some(position) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(position);
        }
        self.order.push_back(key.clone());
    }

    fn forget_cwds(&mut self, cwds: &[PathBuf]) {
        if cwds.is_empty() {
            return;
        }
        self.entries
            .retain(|_, resolved| !cwds.contains(&resolved.cwd));
        self.order.retain(|key| self.entries.contains_key(key));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

pub(super) struct RemoteExecution {
    data: Arc<RemoteSessionData>,
    read_only: bool,
    observations: Mutex<ObservationCache>,
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
            observations: Mutex::new(ObservationCache::default()),
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
        // v19 起绑定行的 `workspace_id` 就是会话归属行的 id（升级把绑定收敛到 `threads`）。
        // 不等说明这条绑定不属于本次要复核的归属：不拿观察到的根替它改判，直接失败关闭，
        // 与本机组合「解析结果 ≠ 归属行」得到同一个结论——登记表已经没有了，那种「另一张
        // 表里的旧登记」的追认也随之消失。
        if binding.workspace_id != owner {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
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
            let cwd =
                peri_acp_types::workspace::binding_cwd(root, &binding.cwd_relative_to_workspace);
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
                cwd,
                root: root.clone(),
                relative_cwd: binding.cwd_relative_to_workspace.clone(),
                discovery_snapshot: Some(snapshot.to_owned()),
            });
        }
        {
            let saved: Discovery =
                serde_json::from_str(snapshot).map_err(|_| WorkspaceError::InvalidBinding)?;
            let cwd = if binding.cwd_relative_to_workspace.as_os_str().is_empty() {
                saved.root.clone()
            } else {
                saved.root.join(&binding.cwd_relative_to_workspace)
            };
            // 恢复准入只要求 cwd 可用且未被 symlink 换位；文件对象身份与 Git 布局
            // 只随快照保存，不参与判定。
            let canonical_cwd = discovery::ensure_cwd_available(&cwd).await?;
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
                cwd: canonical_cwd,
                root: saved.root,
                relative_cwd: binding.cwd_relative_to_workspace.clone(),
                discovery_snapshot: Some(snapshot.to_owned()),
            })
        }
    }
}

#[async_trait]
impl LocalExecutionPort for RemoteExecution {
    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn retains_workspace_observations(&self) -> bool {
        true
    }

    fn forget_workspace_observations(&self, cwds: &[PathBuf]) {
        // 回收是缓存清理，不是删除结果的一部分；锁中毒只放弃回收。
        if let Ok(mut observations) = self.observations.lock() {
            observations.forget_cwds(cwds);
        }
    }

    fn machine_id(&self) -> Result<String> {
        self.environment.machine_id()
    }

    async fn directory_available(&self, cwd: &Path) -> bool {
        match &self.environment {
            RemoteWorkspaceEnvironment::Native => tokio::fs::metadata(cwd)
                .await
                .map(|meta| meta.is_dir())
                .unwrap_or(false),
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
                // 与恢复路径同一形态：空相对路径不通过 `join` 追加尾分隔符。
                cwd: peri_acp_types::workspace::binding_cwd(root, &relative_cwd),
                root: root.clone(),
                relative_cwd,
                discovery_snapshot: Some(snapshot),
            };
            self.observations
                .lock()
                .map_err(|_| WorkspaceError::Unavailable)?
                .record(
                    (workspace_id, resolved.relative_cwd.clone()),
                    resolved.clone(),
                );
            return Ok(resolved);
        }
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
                cwd,
                root: discovery.root,
                relative_cwd,
                discovery_snapshot: Some(snapshot),
            };
            self.observations
                .lock()
                .map_err(|_| WorkspaceError::Unavailable)?
                .record(
                    (workspace_id, resolved.relative_cwd.clone()),
                    resolved.clone(),
                );
            Ok(resolved)
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
            .record(
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

#[cfg(test)]
#[path = "remote_execution_test.rs"]
mod tests;

#[cfg(test)]
impl RemoteExecution {
    pub(super) fn observation_cwds(&self) -> Vec<PathBuf> {
        self.observations
            .lock()
            .expect("observation cache lock poisoned")
            .entries
            .values()
            .map(|resolved| resolved.cwd.clone())
            .collect()
    }
}
