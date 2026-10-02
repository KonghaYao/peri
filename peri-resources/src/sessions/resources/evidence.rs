//! Session execution evidence checks shared by local and remote data homes.

use super::*;

impl SessionResourcesImpl {
    pub(super) async fn load_workspace_for_session(
        &self,
        id: &ThreadId,
        check: BindingRecheck,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        let meta = self.gate.data().load_meta(id).await?;
        let binding = self.gate.data().binding_of(id).await?;
        let missing_directory = !tokio::fs::metadata(&meta.cwd)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        let foreign_machine = self
            .gate
            .data()
            .machine_id_of(id)
            .await?
            .is_some_and(|machine| {
                crate::sessions::machine::current().is_ok_and(|current| machine != current)
            });
        if binding.is_none() && !missing_directory && !foreign_machine {
            if let Ok(resolved) = self
                .gate
                .local()
                .resolve_workspace(Path::new(&meta.cwd))
                .await
            {
                let owner = self.gate.data().workspace_id_of(id).await?;
                if owner == Some(resolved.workspace_id) {
                    return Ok(resolved);
                }
            }
        }
        // History remains addressable by Session ID when its old binding is
        // absent, its directory is gone, or it belongs to another machine.
        // The returned projection carries no execution proof; the separate
        // validate_session gate still checks the immutable evidence.
        if binding.is_none() || missing_directory || foreign_machine {
            let relative_cwd = binding
                .as_ref()
                .map(|binding| binding.cwd_relative_to_workspace.clone())
                .unwrap_or_default();
            let cwd = std::path::PathBuf::from(&meta.cwd);
            let mut root = cwd.clone();
            for _ in relative_cwd.components() {
                root.pop();
            }
            let (project_id, execution_registration_id) = match binding {
                Some(binding) => (binding.project_id, binding.workspace_id),
                None => {
                    use sha2::{Digest, Sha256};
                    let digest = Sha256::digest(id.as_bytes());
                    let identity = uuid::Uuid::from_bytes(
                        digest[..16].try_into().expect("UUID digest prefix"),
                    )
                    .to_string();
                    (
                        identity.parse().expect("project UUID"),
                        identity.parse().expect("workspace UUID"),
                    )
                }
            };
            let workspace_id = self
                .gate
                .data()
                .workspace_id_of(id)
                .await?
                .ok_or_else(|| SessionResourceError::new(SessionResourceErrorKind::NotFound))?;
            return Ok(ResolvedWorkspace {
                project_id,
                workspace_id,
                execution_registration_id,
                cwd,
                root,
                relative_cwd,
                discovery_snapshot: None,
            });
        }
        let resolved = self
            .recheck_binding_of(id, check == BindingRecheck::Full)
            .await?;
        let owner = self
            .gate
            .data()
            .workspace_id_of(id)
            .await?
            .ok_or_else(|| SessionResourceError::new(SessionResourceErrorKind::NotFound))?;
        if resolved.workspace_id != owner {
            return Err(Self::workspace_mismatch());
        }
        Ok(resolved)
    }

    /// 用 canonical 绑定字节做本机复核（`full` 为真时叠一次完整发现快照比对）。
    ///
    /// 绑定字节来自数据面：本机组合是本机 `session_bindings`，远程组合是远端会话行。
    /// 没有绑定行时按 `BindingMissing` 如实失败——执行资格需要一个可复核的绑定。
    pub(super) async fn recheck_binding(
        &self,
        binding: Option<&SessionBinding>,
        full: bool,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        let binding = binding.ok_or_else(|| {
            SessionResourceError::new(SessionResourceErrorKind::Workspace(
                WorkspaceError::BindingMissing,
            ))
        })?;
        self.gate
            .local()
            .validate_binding_value(binding, full)
            .await
            .map_err(execution_failure)
    }

    /// 按 id 取数据面绑定字节后复核（只回答「绑定向哪里」的调用方用这个）。
    pub(super) async fn recheck_binding_of(
        &self,
        id: &ThreadId,
        full: bool,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        let binding = self.gate.data().binding_of(id).await?;
        let resolved = self.recheck_binding(binding.as_ref(), full).await?;
        let owner = self
            .gate
            .data()
            .workspace_id_of(id)
            .await?
            .ok_or_else(|| SessionResourceError::new(SessionResourceErrorKind::NotFound))?;
        if owner != resolved.workspace_id {
            return Err(Self::workspace_mismatch());
        }
        let saved = self.gate.data().binding_discovery_snapshot(id).await?;
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
                    return Err(Self::workspace_mismatch());
                }
            }
            None if full && matches!(self.home, SessionDataHome::RemoteStore) => {
                self.gate
                    .data()
                    .complete_legacy_binding_discovery(
                        id,
                        binding
                            .as_ref()
                            .expect("binding checked by recheck_binding"),
                        &resolved,
                    )
                    .await?;
            }
            None => {
                return Err(SessionResourceError::new(
                    SessionResourceErrorKind::Workspace(WorkspaceError::InvalidBinding),
                ))
            }
        }
        Ok(resolved)
    }

    /// `Missing` 与 `LegacyConfirmed` 的差别是本机来源证据：无绑定、无父会话、无 frozen，
    /// 且保存的绝对 cwd 落在本机已登记工作区内，才表达为 legacy 历史；其余的无
    /// 绑定状态（外来会话、登记缺失）不冒充 legacy。
    ///
    /// 这份证据**只属于本机组合**：`legacy_confirmed` 读的是本机 `threads` / `session_bindings`
    /// / `workspaces`，远端组合里会话行与它的 cwd 都在远端，本机根本没有可读的来源证据。
    /// 因此远端组合由门面**固定为 false**（端口文档同此），而不是去本机表里碰运气：
    /// 本机恰有一条同 id 的行就会把远端会话判成 legacy。
    pub(super) async fn classify_binding(
        &self,
        id: &ThreadId,
        state: BindingState,
    ) -> SessionResourceResult<BindingState> {
        if !matches!(state, BindingState::Missing) {
            return Ok(state);
        }
        if matches!(self.home, SessionDataHome::RemoteStore) {
            return Ok(state);
        }
        if self
            .gate
            .local()
            .legacy_confirmed(id)
            .await
            .map_err(execution_failure)?
        {
            return Ok(BindingState::LegacyConfirmed);
        }
        Ok(state)
    }
}
