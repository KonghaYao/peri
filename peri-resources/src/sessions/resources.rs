//! 会话数据资源门面：工作区校验、读写权限与持久化结清。
//! 执行所有权由 peri-sdk 维护，本模块不登记或认领执行者。

mod claim;
mod control;
mod evidence;
mod gate;
mod lifecycle;
mod oauth_credentials;
mod work;

use std::path::Path;
#[cfg(not(target_os = "emscripten"))]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    AccessMode, BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, CloseSettlement,
    DataCapabilities, ExecutionAvailability, ForkSnapshot, FrozenSnapshotBytes, FrozenState,
    NewSession, NewSessionDraft, PersistenceRecovery, RewindBoundary, SessionAvailability,
    SessionInitialization, SessionMetaPatch, SessionResourceError, SessionResourceErrorKind,
    SessionResourceResult, SessionResources, SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::thread::{AgentStatus, ThreadId, ThreadMeta};
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding, WorkspaceError,
};

use super::data::{ensure_child_relation, ChildResumeRecord, SessionDataPort};
#[cfg(target_os = "emscripten")]
use super::failure::execution_failure;
use super::failure::{invalid_input, not_found};
use super::local_port::LocalExecutionPort;
#[cfg(not(target_os = "emscripten"))]
use super::sqlite_store::execution_failure;
#[cfg(not(target_os = "emscripten"))]
use super::sqlite_store::{LocalExecution, ReadOnlyThreadStoreError};

use claim::ChildResumeClaimHandle;
use gate::MutationGate;
use lifecycle::{Lifecycle, LifecycleState};

/// 排空与关闭的有界等待。
///
/// 本机写入是短事务（单条 SQL 或一个 `BEGIN IMMEDIATE`），超过这个预算说明有写入卡住；
/// 此时报告未结清，而不是无限期等待一个外部 future。
const SETTLE_WAIT: Duration = Duration::from_secs(10);

/// 会话数据存放位置，用于选择工作区证据校验规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sessions) enum SessionDataHome {
    LocalLibrary,
    RemoteStore,
}

struct DraftInitialization {
    id: ThreadId,
    gate: MutationGate,
}

#[async_trait]
impl SessionInitialization for DraftInitialization {
    fn thread_id(&self) -> &ThreadId {
        &self.id
    }

    async fn commit_frozen(&self, frozen: &FrozenSnapshotBytes) -> SessionResourceResult<()> {
        self.gate
            .with_exclusive(&self.id, || {
                self.gate.data().commit_frozen(&self.id, frozen)
            })
            .await
    }

    async fn abandon(self: Arc<Self>) -> SessionResourceResult<()> {
        self.gate.ensure_session_write()?;
        self.gate
            .with_registration(&self.id, || {
                self.gate.data().revoke_unpublished_draft(&self.id)
            })
            .await
    }
}

/// 会话资源门面（生产实现）。
///
/// 构造即确定访问模式与后端：[`Self::open`] 写打开（必要时原地升级 schema），
/// [`Self::open_existing_read_only`] 只读打开（不建库、不建表、不建锁文件）。
pub struct SessionResourcesImpl {
    gate: MutationGate,
    /// 关闭生命周期与 [`MutationGate`] 共享：`Closing` 起停止新写入，只有真实检查全部
    /// 结清并关闭数据面之后才确认 `Closed`（见 [`Self::close`]）。
    lifecycle: Lifecycle,
    /// 串行化关闭确认：并发关闭必须依次看到真实结论，不能两个都「从头开始」而重复关闭
    /// 同一个连接，也不能把另一个调用的未确认状态当成成功。
    close_confirm: tokio::sync::Mutex<()>,
    credential_operations: Arc<tokio::sync::RwLock<()>>,
    /// 会话数据的存放位置：决定 `create_session` 是几次提交（见 [`SessionDataHome`]）。
    home: SessionDataHome,
}

impl SessionResourcesImpl {
    /// 打开或创建会话库，原地升级已知旧 schema 并保留历史数据。
    #[cfg(not(target_os = "emscripten"))]
    pub async fn open(db_path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        Ok(Self::from_local(LocalExecution::open(db_path).await?))
    }

    /// 以只读方式打开已存在的会话库；不创建目录、库、schema 或锁文件。
    #[cfg(not(target_os = "emscripten"))]
    pub async fn open_existing_read_only(
        db_path: impl AsRef<Path>,
    ) -> Result<Self, ReadOnlyThreadStoreError> {
        Ok(Self::from_local(
            LocalExecution::open_existing_read_only(db_path).await?,
        ))
    }

    /// 默认数据库位置 `~/.peri/threads/threads.db`；不创建目录、数据库或连接。
    #[cfg(not(target_os = "emscripten"))]
    pub fn default_database_path() -> anyhow::Result<PathBuf> {
        LocalExecution::default_database_path()
    }

    /// 与迁移桥共享同一个库句柄（迁移期唯一装配点 `SqliteThreadStore::open_shared*` 使用）。
    ///
    /// 本机组合：数据面与执行面由同一个库句柄回答（同一条连接真相），两个端口因此只是
    /// 同一实现的两张面孔。
    #[cfg(not(target_os = "emscripten"))]
    pub(in crate::sessions) fn from_local(local: LocalExecution) -> Self {
        let data = Arc::new(local.data_port());
        Self::from_ports(data, Arc::new(local), SessionDataHome::LocalLibrary)
    }

    /// 组合层装配点：数据面 + 本机执行面，两者由调用方决定指向哪个后端。
    ///
    /// 门面不持有任何后端判断，只按 `home`（组合层给出的**事实**）决定 `create_session`
    /// 是一步还是两步；公开行为仍只有 [`SessionResources`] 这一套。
    pub(in crate::sessions) fn from_ports(
        data: Arc<dyn SessionDataPort>,
        local: Arc<dyn LocalExecutionPort>,
        home: SessionDataHome,
    ) -> Self {
        let lifecycle = Lifecycle::new();
        let gate = MutationGate::new(data, local, lifecycle.clone(), home);
        Self {
            gate,
            lifecycle,
            close_confirm: tokio::sync::Mutex::new(()),
            credential_operations: Arc::new(tokio::sync::RwLock::new(())),
            home,
        }
    }

    async fn execution_availability(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<ExecutionAvailability> {
        let local = self.gate.local();
        if local.is_read_only() {
            return Ok(ExecutionAvailability::ReadOnlyStore);
        }
        if !self.gate.data().session_exists(id).await? {
            return Err(not_found());
        }
        if let Some(machine_id) = self.gate.data().machine_id_of(id).await? {
            if machine_id
                != local.machine_id().map_err(|_| {
                    crate::sessions::failure::unavailable("machine identity is not initialized")
                })?
            {
                return Ok(ExecutionAvailability::WorkspaceUnavailable);
            }
        }
        let meta = self.gate.data().load_meta(id).await?;
        if matches!(self.home, SessionDataHome::RemoteStore)
            && self
                .gate
                .data()
                .binding_discovery_snapshot(id)
                .await?
                .is_none()
        {
            return Ok(ExecutionAvailability::WorkspaceUnavailable);
        }
        if !local.directory_available(Path::new(&meta.cwd)).await {
            return Ok(ExecutionAvailability::WorkspaceUnavailable);
        }
        if matches!(self.home, SessionDataHome::RemoteStore) {
            match self.recheck_binding_of(id, true).await {
                Ok(_) => {}
                Err(error) if matches!(error.kind(), SessionResourceErrorKind::Workspace(_)) => {
                    return Ok(ExecutionAvailability::WorkspaceUnavailable);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(ExecutionAvailability::Available)
    }

    /// 完整创建的重试：不可变事实一致时只重新建立执行准入。
    ///
    /// 前提（又是同一次创建、工作区证据仍然一致）成立时补上准入；否则保留 identity 并
    /// 如实报告「已保存、未准入」——既不重造 binding/frozen，也不谎称「确定未创建」。
    async fn admit_saved_creation(&self, input: &NewSession) -> SessionResourceResult<()> {
        let id = &input.thread_id;
        let saved = self.gate.data().load_snapshot(id).await?;
        let created_at = chrono::DateTime::parse_from_rfc3339(&input.created_at)
            .map_err(|_| invalid_input("invalid session creation timestamp"))?;
        let snapshot_at = input
            .meta
            .snapshot_at_message_id
            .map(|message| message.as_uuid().to_string());
        if saved.meta.id != *id
            || saved.meta.created_at != created_at
            || saved.meta.cwd != input.meta.cwd
            || saved.meta.parent_thread_id != input.meta.parent_thread_id
            || saved.meta.snapshot_at_message_id != snapshot_at
            || saved.binding != BindingState::Bound(input.binding.clone())
            || saved.frozen != FrozenState::Present(input.frozen.clone())
        {
            return Err(SessionResourceError::conflict(
                "session identity has different immutable creation facts",
            ));
        }
        if !matches!(
            self.execution_availability(id).await?,
            ExecutionAvailability::Available
        ) {
            return Err(SessionResourceError::saved_but_not_admitted(id.clone()));
        }
        self.recheck_binding(Some(&input.binding), false).await?;
        Ok(())
    }

    fn workspace_mismatch() -> SessionResourceError {
        SessionResourceError::new(SessionResourceErrorKind::Workspace(
            WorkspaceError::ExecutionBindingMismatch,
        ))
    }

    /// 测试用：本机组合背后的 SQLite 连接池（逐条构造事实的夹具使用）。
    #[cfg(all(test, not(target_os = "emscripten")))]
    pub(super) fn local_pool(&self) -> &sqlx::SqlitePool {
        self.gate
            .local()
            .sqlite_pool()
            .expect("local composition always has a SQLite pool")
    }
}

// ─── 门面实现 ─────────────────────────────────────────────────────────────────

#[async_trait]
impl SessionResources for SessionResourcesImpl {
    async fn load_resource_owner_facts(
        &self,
        id: &ThreadId,
        previous_lifecycle: u64,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::ResourceOwnerFacts> {
        self.gate
            .load_resource_owner_facts(id, previous_lifecycle)
            .await
    }
    async fn load_work_revision(&self, id: &ThreadId) -> SessionResourceResult<u64> {
        self.gate.load_work_revision(id).await
    }
    async fn load_work_availability(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkAvailability> {
        self.gate.load_work_availability(id).await
    }
    async fn load_work_delivery(
        &self,
        query: &peri_acp_types::session_resources::work::WorkDeliveryQuery,
    ) -> SessionResourceResult<Option<peri_acp_types::session_resources::work::DeliveryRecord>>
    {
        self.gate.load_work_delivery(query).await
    }
    async fn load_work_command(
        &self,
        query: &peri_acp_types::session_resources::work::WorkCommandQuery,
    ) -> SessionResourceResult<Option<peri_acp_types::session_resources::work::OwnedWorkCommand>>
    {
        self.read_work_command(query).await
    }
    async fn load_session_work(
        &self,
        query: &peri_acp_types::session_resources::work::WorkQuery,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkSnapshot> {
        self.read_session_work(query).await
    }
    async fn apply_work_mutation(
        &self,
        command: &peri_acp_types::session_resources::work::PreparedWorkCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkReceipt> {
        self.write_work_mutation(command).await
    }
    async fn resolve_work_mutation(
        &self,
        command: &peri_acp_types::session_resources::work::PreparedWorkCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::work::WorkResolution> {
        self.reconcile_work_mutation(command).await
    }
    async fn load_session_control(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::ControlState> {
        self.read_session_control(id).await
    }

    async fn apply_session_control(
        &self,
        command: &peri_acp_types::session_resources::ControlCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::ControlReceipt> {
        self.write_session_control(command).await
    }

    async fn resolve_session_control(
        &self,
        command: &peri_acp_types::session_resources::ControlCommand,
    ) -> SessionResourceResult<peri_acp_types::session_resources::ControlResolution> {
        self.reconcile_session_control(command).await
    }

    fn oauth_credentials_for_workspace(
        &self,
        workspace_id: peri_acp_types::workspace::WorkspaceId,
    ) -> Option<Arc<dyn peri_acp_types::oauth_credentials::OAuthCredentialPort>> {
        self.gate
            .data()
            .clone()
            .oauth_credentials_for_workspace(workspace_id)
            .map(|inner| {
                Arc::new(oauth_credentials::LifecycleCredentials::new(
                    inner,
                    self.lifecycle.clone(),
                    self.credential_operations.clone(),
                ))
                    as Arc<dyn peri_acp_types::oauth_credentials::OAuthCredentialPort>
            })
    }
    // ── 能力与准入 ──

    async fn inspect_availability(
        &self,
        session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability> {
        let read_only = self.gate.local().is_read_only();
        let access = if read_only {
            AccessMode::ReadOnly
        } else {
            AccessMode::ReadWrite
        };
        let capabilities = if read_only {
            DataCapabilities::HistoryReadOnly
        } else {
            DataCapabilities::Complete
        };
        let execution = match session {
            None => None,
            Some(id) => Some(self.execution_availability(id).await?),
        };
        Ok(SessionAvailability {
            access,
            capabilities,
            execution,
        })
    }

    async fn resolve_workspace(&self, cwd: &Path) -> SessionResourceResult<ResolvedWorkspace> {
        self.gate.ensure_registration_write()?;
        self.gate
            .local()
            .resolve_workspace(cwd)
            .await
            .map_err(execution_failure)
    }

    async fn validate_session(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        // 一次准入的权威复核：关系、关键文件对象加一次完整发现快照比对。
        let resolved = self.recheck_binding_of(id, true).await?;
        if &resolved != workspace {
            return Err(Self::workspace_mismatch());
        }
        Ok(())
    }

    async fn finish_close(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.gate.ensure_recovery_write()?;
        self.gate.finish_close(id).await
    }

    async fn close_settlement(&self, id: &ThreadId) -> SessionResourceResult<CloseSettlement> {
        self.gate.ensure_recovery_permitted()?;
        self.gate.data().close_settlement(id).await
    }

    // ── 创建与接纳 ──

    async fn create_session(&self, input: &NewSession) -> SessionResourceResult<()> {
        self.gate.ensure_registration_write()?;
        if self.gate.data().session_exists(&input.thread_id).await? {
            return self.admit_saved_creation(input).await;
        }
        let workspace = self.recheck_binding(Some(&input.binding), false).await?;
        self.gate
            .with_registration(&input.thread_id, || {
                self.gate
                    .data()
                    .save_new_session_in_workspace(input, &workspace)
            })
            .await?;
        self.admit_saved_creation(input).await
    }

    async fn abandon_initialization(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.gate.ensure_session_write()?;
        self.gate
            .with_registration(id, || self.gate.data().revoke_unpublished_session(id))
            .await
    }

    async fn begin_initialization(
        &self,
        draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionInitialization>> {
        self.gate.ensure_registration_write()?;
        let workspace = self.recheck_binding(Some(&draft.binding), false).await?;
        self.gate
            .with_registration(&draft.thread_id, || {
                self.gate
                    .data()
                    .save_new_session_draft_in_workspace(draft, &workspace)
            })
            .await?;
        Ok(Arc::new(DraftInitialization {
            id: draft.thread_id.clone(),
            gate: self.gate.clone(),
        }))
    }

    async fn discard_incomplete_initialization(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.gate
            .with_exclusive(id, || async {
                if !matches!(
                    self.gate.data().load_binding(id).await?,
                    BindingState::Bound(_)
                ) {
                    return Err(SessionResourceError::conflict(
                        "session is not a bound draft",
                    ));
                }
                self.gate.data().revoke_unpublished_draft(id).await
            })
            .await
    }

    async fn adopt_legacy_session(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.gate.ensure_session_write()?;
        if matches!(self.home, SessionDataHome::RemoteStore) {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Unsupported,
            ));
        }
        self.gate
            .data()
            .adopt_legacy_session(id, saved_cwd, workspace, frozen)
            .await
    }

    // ── 读取 ──

    async fn load_session_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot> {
        let mut snapshot = self.gate.data().load_snapshot(id).await?;
        snapshot.binding = self.classify_binding(id, snapshot.binding).await?;
        Ok(snapshot)
    }

    async fn load_session_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState> {
        let state = self.gate.data().load_binding(id).await?;
        self.classify_binding(id, state).await
    }

    async fn validate_bound_workspace(
        &self,
        id: &ThreadId,
        check: BindingRecheck,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        self.load_workspace_for_session(id, check).await
    }

    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        self.gate.data().load_session_history(id).await
    }

    async fn load_session_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        self.gate.data().load_meta(id).await
    }

    async fn session_environment_id(&self, id: &ThreadId) -> SessionResourceResult<Option<String>> {
        self.gate.data().machine_id_of(id).await
    }
    async fn session_workspace_id(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<peri_acp_types::workspace::WorkspaceId>> {
        self.gate.data().workspace_id_of(id).await
    }

    async fn list_machines(
        &self,
    ) -> SessionResourceResult<Vec<peri_acp_types::workspace::MachineInfo>> {
        self.gate.data().list_machines().await
    }

    async fn list_workspaces(
        &self,
        machine_id: &str,
    ) -> SessionResourceResult<Vec<peri_acp_types::workspace::WorkspaceInfo>> {
        self.gate.data().list_workspaces(machine_id).await
    }

    async fn rename_machine(&self, machine_id: &str, name: &str) -> SessionResourceResult<()> {
        self.gate.data().rename_machine(machine_id, name).await
    }

    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.gate.data().list_sessions(query).await
    }

    async fn list_archived_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.gate.data().list_archived_sessions(query).await
    }

    async fn set_session_archived(
        &self,
        id: &ThreadId,
        archived: bool,
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().set_session_archived(id, archived))
            .await
    }

    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.gate.data().list_children(parent).await
    }

    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.gate.data().list_session_tree(root).await
    }

    // ── 写入 ──

    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().append_history(id, payloads))
            .await
    }

    async fn append_reminder_if_absent(
        &self,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &peri_acp_types::system_reminder::TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        self.gate
            .with_mutation(id, || {
                self.gate
                    .data()
                    .append_reminder_if_absent(id, message_id, reminder)
            })
            .await
    }

    async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().mark_session_closing(id))
            .await
    }

    async fn is_session_closing(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.gate.data().is_session_closing(id).await
    }

    async fn save_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()> {
        self.gate.ensure_registration_write()?;
        let data = self.gate.data();
        if !data.session_exists(&fork.source_id).await? {
            return Err(not_found());
        }
        if data.session_exists(&fork.target.thread_id).await? {
            return self.admit_saved_creation(&fork.target).await;
        }
        let workspace = self
            .recheck_binding(Some(&fork.target.binding), false)
            .await?;
        self.gate
            .with_registration(&fork.target.thread_id, || {
                self.gate.data().save_fork_in_workspace(fork, &workspace)
            })
            .await?;
        self.admit_saved_creation(&fork.target).await
    }

    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()> {
        ensure_child_relation(child)?;
        self.gate.ensure_registration_write()?;
        self.recheck_binding(Some(&child.target.binding), false)
            .await?;
        self.gate
            .with_mutation(&child.root_id, || self.gate.data().save_child(child))
            .await
    }

    async fn claim_child_resume(
        &self,
        child: &ThreadId,
        root: &ThreadId,
    ) -> SessionResourceResult<Box<dyn ChildResumeClaim>> {
        self.gate.ensure_session_write()?;
        if self.gate.data().session_root(child).await? != *root {
            return Err(invalid_input(
                "child session does not belong to the claimed root",
            ));
        }
        // 认领在写侧门禁内完成「读状态 + 写 active」：并发认领里只有一个能看到非 active。
        let previous = self
            .gate
            .with_exclusive(root, || async {
                let previous = self.gate.data().load_child_resume_record(child).await?;
                if previous.status.is_active() {
                    return Err(invalid_input("child session is still active"));
                }
                self.gate
                    .data()
                    .store_child_resume_record(
                        child,
                        &ChildResumeRecord {
                            status: AgentStatus::Active,
                            claimed: true,
                        },
                    )
                    .await?;
                Ok(previous)
            })
            .await?;
        Ok(Box::new(ChildResumeClaimHandle::new(
            self.gate.clone(),
            child.clone(),
            previous,
        )))
    }

    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().apply_compaction(id, change))
            .await
    }

    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || {
                self.gate.data().apply_message_projections(id, updates)
            })
            .await
    }

    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().rewind_history(id, boundary))
            .await
    }

    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().remove_history_entries(id, ids))
            .await
    }

    async fn update_session_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().update_meta(id, patch))
            .await
    }

    async fn delete_session_tree(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.gate
            .with_mutation(id, || self.gate.data().delete_tree(id))
            .await
    }

    async fn recover_session_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        self.gate.recover(id).await
    }

    async fn drain_persistence(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.gate.drain(id).await
    }
}

#[path = "resources/deployment.rs"]
mod deployment;

#[cfg(test)]
#[path = "resources_test.rs"]
mod tests;
