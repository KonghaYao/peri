//! 远程会话数据 adapter：把 [`SessionDataPort`] 的会话行为落到远端 schema 上。
//!
//! ## 本阶段落地范围（C-03 第一、二批）
//!
//! | 行为 | 状态 |
//! | --- | --- |
//! | `load_snapshot` / `load_meta` / `load_binding` / `load_session_history` | 已实现（一致读取；flags 由一致快照一并返回） |
//! | `list_sessions`（scoped 分页）/ `list_children` / `list_session_tree` | 已实现 |
//! | `save_new_session` | 已实现（meta + 绑定 + frozen 一次落库） |
//! | `save_fork` | 已实现（映射后的 payload + flags，source 不变） |
//! | `save_child` | 已实现（父子/根归属 + 继承区 + root frozen 原文） |
//! | `update_meta`（title/status/cancel_policy/config 定向更新） | 已实现 |
//! | `append_history` / `apply_message_projections` / `apply_compaction` | 已实现（[`super::session_history`]：批内守卫，整批生效或整批不生效） |
//! | `rewind_history`（显式两边界）/ `remove_history_entries` | 已实现（同上；未知截止点保持无变更语义） |
//! | `delete_tree` / `revoke_unpublished_session` | 已实现（[`super::session_lifecycle`]；远端无墓碑/执行行，删除是刻意删除数据事实） |
//! | `load_child_resume_record` / `store_child_resume_record` | 已实现（`agent_status` + 由状态派生的认领标记） |
//! | `drain` | 已实现为「无队列可排空，但未结清不算已排空」（见方法文档） |
//! | `close` | 已实现（真正关闭连接，之后写入明确失败） |
//! | `recover_persistence` | 已实现：本机已无可求证的 durable 记录，会话数据可读即 `Recovered`（跨进程未决判定随 v10 撤销） |
//!
//! 写入路径：**每次调用唯一操作 id + 资格先于效果**（远端账本 `peri_op_ledger` 的资格写入
//! 与业务效果在同一个托管批里同生共死）。操作 id 不由内容派生，因此同内容的第二次、第三次
//! 领域调用都是新操作（状态 A→B→A、标题 x→y→x 不再被当成重放丢弃）；输入摘要只用于一致性
//! 校验。v10 撤销本机操作日志后，不再有「发送前本机落盘、确定终态才结清」这一步，未结清只
//! 在当前实例的 persistence gate 上表达（见 `recover_persistence` 的方法文档）。等价的公开行为仍只有门面暴露的
//! 那 33 条——adapter 不另立一套平行行为。
//!
//! Workspace 归属与执行快照保存在远端；文件系统发现和进程内运行句柄由
//! `RemoteExecution` 持有（见 `sessions::local_port`）。
//!
//! 打开的两种访问模式：
//!
//! - 可写打开：读回身份（未初始化则先建身份），只在缺表或缺必要索引时补齐 schema；
//! - 只读打开：只读回身份，不写任何东西；若 store 尚无本任务 schema（身份或会话表缺失），
//!   直接 `Unsupported`——没有 schema 就没有会话事实可读，也不越权建表。
//!
//! ## 边界（远程不做本机的事）
//!
//! - 不持有本机执行事实：运行句柄及未结清标记只在当前实例内，远端不持久化这些事实；
//! - 不解析本机目录、不发执行资格、不判定 legacy：`LegacyConfirmed` 与执行准入由门面与
//!   执行面按本机证据判定（这也是 `load_binding` 只回答绑定事实的原因）；

use super::generation::{ConnectionFactory, ConnectionGate};
use super::schema::StoreId;
use super::session_sql;
use super::sql::StatementSpec;
use crate::sessions::data::{ChildResumeRecord, SessionDataPort};
use async_trait::async_trait;
use peri_acp_types::history_page::{
    HistoryCursor, HistoryForwardCursor, HistoryPage, HistoryPageBounds, HistorySegment,
};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    ActiveSessionSnapshot, BindingState, ChildSnapshot, ForkSnapshot, FrozenSnapshotBytes,
    FrozenState, NewSession, PersistenceRecovery, RewindBoundary, RewindCandidate, RewindOutcome,
    SessionMetaPatch, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
    SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta, WorkState};
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use turso_serverless::Value;
#[cfg(test)]
#[path = "session_data_fixture.rs"]
mod fixtures;
#[path = "session_close.rs"]
mod session_close;

mod connection_lifecycle;
mod construction;
mod ownership;
mod recovery;

use connection_lifecycle::{connection_closed, ConnectionSlot};

/// 本次打开对远端 store 身份做了什么：首次登记资格的唯一证据。
///
/// 「远端此前为空」只由**本次打开建立身份**证明；读到既有身份说明这个 store 已经有数据，
/// 无论本机是否第一次见到它，都不构成自动认领的理由。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StoreInitialization {
    /// 远端此前已有本任务 schema（本次只读回身份）。
    Existing,
    /// 远端此前明确为空，本次打开建立了 store 身份。
    CreatedByThisOpen,
}

#[path = "session_open.rs"]
mod session_open;
pub(super) use session_open::{
    open_step, open_verdict, probe_shape, verify_legacy_open_identity, OpenStep,
};

/// 远端会话数据 adapter：一个已初始化（或已读回身份）的远程 store 上的会话行为。
///
/// 连接在关闭**确认**之前一直活着：服务期间留在槽里；关闭一开始就离开业务路径、移进关闭
/// 句柄（[`connection_lifecycle::ClosingConnection`]），由它保留真实资源并允许重试，`close` 成功返回才算确认。
/// 一次已经发出的在途请求被丢弃（超时、取消）之后，用到它的那一代连接被记为失效，
/// **下一次访问按同一份打开事实重建**（见 [`Self::store`]）。关闭开始之后（无论上一次成功
/// 与否）再没有重连：槽里没有可服务的连接时如实返回关闭错误，绝不用一次重连把关闭事实盖掉。
///
/// 本机**不再**持有远端操作的日志（v10 删除了 `session_remote_operations`）：远端账本
/// （`peri_op_ledger`）仍按「资格先于效果」写，但它是**远端**事实，本机不复制。
/// 普通 mutation 的写入确认保留；它不提供 Agent 执行恢复，不能把内存 gate 当成持久日志。
pub(super) struct RemoteSessionData {
    pub(super) machine_id: String,
    /// 连接的生命周期槽位：服务中，或关闭中（含已确认关闭）。
    slot: RwLock<ConnectionSlot>,
    /// 建立连接的地方（本 crate 唯一持有凭证处）；重建不改变打开事实。
    factory: Arc<dyn ConnectionFactory>,
    /// 连接代际门禁：哪一代已经不可信。
    gate: Arc<ConnectionGate>,
    store_id: StoreId,
    /// 压缩前的形状（`v2` 契约，10|11）：只读打开下它没有当前形状的归属列与机器目录，
    /// 读路径按老形状服务。写打开会先升级，升级后的实例不再带这一位。
    pub(super) legacy_shape: bool,
    /// thread → root 解析缓存：父关系创建后不变（本 adapter 不提供改父行为），因此
    /// 同一条会话只需一次远端上溯；解析失败不缓存，避免把网络失败固化成事实。
    roots: RwLock<HashMap<ThreadId, ThreadId>>,
}

impl RemoteSessionData {
    /// metadata 投影代数：打开时的身份读回判为旧代只读形状（`legacy_shape`）时，
    /// `threads` 没有两列工作快照，读取改用同形占位列；codec 仍是同一套。
    pub(super) fn meta_projection(&self) -> session_sql::MetaProjection {
        session_sql::MetaProjection::from_legacy_shape(self.legacy_shape)
    }

    /// 操作所属 root：已有会话沿父链上溯（带缓存），解析失败退回自身。
    ///
    /// 退回自身只影响未决阻塞的范围（这一条会话而不是整棵树），不会把未知写当成已知：
    /// 该记录仍然是未结清状态，门禁照常阻塞该会话。失败结果**不**进缓存。
    pub(super) async fn root_for(&self, id: &ThreadId) -> ThreadId {
        if let Some(root) = self.roots.read().await.get(id) {
            return root.clone();
        }
        match self.root_of(id).await {
            Ok(root) => {
                self.roots.write().await.insert(id.clone(), root.clone());
                root
            }
            Err(_) => id.clone(),
        }
    }
}

pub(super) fn not_found() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::NotFound)
}
pub(super) fn invalid_input(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
        detail: detail.to_owned(),
    })
}

/// 本阶段尚未落地的行为：明确失败，并留下行为名便于诊断（不含任何会话内容）。
pub(super) fn unsupported_behavior(behavior: &'static str) -> SessionResourceError {
    tracing::debug!(behavior, "remote session data behavior is not implemented");
    SessionResourceError::new(SessionResourceErrorKind::Unsupported)
}

#[async_trait]
impl SessionDataPort for RemoteSessionData {
    async fn finish_close(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.finish_session_close(id).await
    }

    async fn close_settlement(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::CloseSettlement> {
        self.store().await?.close_settlement(id).await
    }

    fn oauth_credentials_for_workspace(
        self: Arc<Self>,
        workspace_id: peri_acp_types::workspace::WorkspaceId,
    ) -> Option<Arc<dyn peri_acp_types::oauth_credentials::OAuthCredentialPort>> {
        // 压缩前的形状里凭证只有 machine 作用域，给不出 workspace 归属。
        if self.legacy_shape {
            return None;
        }
        Some(Arc::new(super::oauth_credentials::RemoteOAuthCredentials(
            self,
            workspace_id,
        )))
    }

    async fn machine_id_of(&self, id: &ThreadId) -> SessionResourceResult<Option<String>> {
        self.read_machine_id_of(id).await
    }
    async fn workspace_id_of(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<peri_acp_types::workspace::WorkspaceId>> {
        self.read_workspace_id_of(id).await
    }
    async fn list_machines(
        &self,
    ) -> SessionResourceResult<Vec<peri_acp_types::workspace::MachineInfo>> {
        self.catalog_machines().await
    }
    async fn list_workspaces(
        &self,
        machine_id: &str,
    ) -> SessionResourceResult<Vec<peri_acp_types::workspace::WorkspaceInfo>> {
        self.catalog_workspaces(machine_id).await
    }
    async fn rename_machine(&self, machine_id: &str, name: &str) -> SessionResourceResult<()> {
        self.catalog_rename_machine(machine_id, name).await
    }
    async fn save_new_session(&self, input: &NewSession) -> SessionResourceResult<()> {
        self.write_new_session(input).await
    }
    async fn save_new_session_in_workspace(
        &self,
        input: &NewSession,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        self.write_new_session_in_workspace(input, workspace).await
    }

    async fn save_new_session_draft(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
    ) -> SessionResourceResult<()> {
        self.write_new_session_draft(draft).await
    }
    async fn save_new_session_draft_in_workspace(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        self.write_new_session_draft_in_workspace(draft, workspace)
            .await
    }

    async fn commit_frozen(
        &self,
        id: &ThreadId,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.write_commit_frozen(id, frozen).await
    }

    async fn revoke_unpublished_session(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.revoke_unpublished(id).await
    }

    async fn revoke_unpublished_draft(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.revoke_unpublished_draft(id).await
    }

    async fn adopt_legacy_session(
        &self,
        _id: &ThreadId,
        _saved_cwd: &str,
        _workspace: &ResolvedWorkspace,
        _frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        // Old remote sessions without immutable evidence remain history-only.
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
    }

    async fn load_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot> {
        self.read_snapshot(id).await
    }

    async fn load_active_snapshot(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<ActiveSessionSnapshot> {
        self.read_active_snapshot(id).await
    }

    async fn load_frozen_state(&self, id: &ThreadId) -> SessionResourceResult<FrozenState> {
        self.read_frozen_state(id).await
    }

    async fn inherited_archive_root(
        &self,
        id: &ThreadId,
        source: &ThreadId,
        anchor: MessageId,
    ) -> SessionResourceResult<peri_acp_types::context_archive::ArchiveRoot> {
        self.read_inherited_archive_root(id, source, anchor).await
    }

    async fn read_archive_metadata(
        &self,
        id: &ThreadId,
        query: &peri_acp_types::context_archive::ArchiveMetadataQuery,
    ) -> SessionResourceResult<peri_acp_types::context_archive::ArchiveMetadataPage> {
        self.read_archive_metadata_page(id, query).await
    }
    async fn read_archive_chunk(
        &self,
        id: &ThreadId,
        query: &peri_acp_types::context_archive::ArchiveChunkQuery,
    ) -> SessionResourceResult<peri_acp_types::context_archive::ArchiveChunk> {
        self.read_archive_body_chunk(id, query).await
    }

    async fn read_archive_chunks(
        &self,
        id: &ThreadId,
        batch: &peri_acp_types::context_archive::ArchiveChunkBatch,
    ) -> SessionResourceResult<Vec<peri_acp_types::context_archive::ArchiveChunk>> {
        self.read_archive_body_chunks(id, batch).await
    }

    async fn load_message_flags(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<HashMap<MessageId, MessageFlags>> {
        self.read_message_flags(id, ids).await
    }

    async fn load_entry_positions(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<HashMap<MessageId, u64>> {
        self.read_active_entry_positions(id).await
    }

    async fn load_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState> {
        self.read_binding_state(id).await
    }

    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        self.read_history(id).await
    }

    async fn load_rewind_candidates(
        &self,
        id: &ThreadId,
        limit: usize,
    ) -> SessionResourceResult<Vec<RewindCandidate>> {
        self.read_rewind_candidates(id, limit).await
    }

    async fn load_history_page(
        &self,
        id: &ThreadId,
        cursor: Option<&HistoryCursor>,
        limit: usize,
        byte_budget: usize,
    ) -> SessionResourceResult<HistoryPage> {
        self.read_history_page(id, cursor, limit, byte_budget).await
    }

    async fn history_page_bounds(&self, id: &ThreadId) -> SessionResourceResult<HistoryPageBounds> {
        self.read_history_page_bounds(id).await
    }

    async fn load_history_segment(
        &self,
        id: &ThreadId,
        cursor: Option<&HistoryForwardCursor>,
        limit: usize,
        byte_budget: usize,
    ) -> SessionResourceResult<HistorySegment> {
        self.read_history_segment(id, cursor, limit, byte_budget)
            .await
    }

    async fn load_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        self.read_meta(id).await
    }

    async fn session_exists(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.exists(id).await
    }

    async fn binding_of(&self, id: &ThreadId) -> SessionResourceResult<Option<SessionBinding>> {
        self.binding_of(id).await
    }

    async fn binding_discovery_snapshot(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<String>> {
        self.read_binding_discovery_snapshot(id).await
    }

    async fn session_root(&self, id: &ThreadId) -> SessionResourceResult<ThreadId> {
        // 远端父链的根；上溯失败按「解析不出」退回自身（范围变窄，但不会把未知当成已知）。
        Ok(self.root_for(id).await)
    }

    async fn recovery_scope(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::recovery::RecoveryScope> {
        // 与本地 adapter 同语义：先严格解析根（远端不退回自身），再读根子树的 working 候选。
        self.read_recovery_scope(id).await
    }

    async fn interrupted_threads(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<peri_acp_types::recovery::InterruptedThread>> {
        self.read_interrupted_threads(id).await
    }

    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.read_page(query).await
    }
    async fn list_archived_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.read_page_by_archive(query, true).await
    }
    async fn set_session_archived(
        &self,
        id: &ThreadId,
        archived: bool,
    ) -> SessionResourceResult<()> {
        let store = self.store().await?;
        let row = store
            .fetch_row(&StatementSpec::new(
                "SELECT parent_thread_id, frozen_context FROM threads WHERE id = ?1",
                vec![Value::Text(id.clone())],
            ))
            .await?
            .ok_or_else(not_found)?;
        if !matches!(row.first(), Some(Value::Null)) {
            return Err(invalid_input("child sessions cannot be archived"));
        }
        if !matches!(row.get(1), Some(Value::Text(_))) {
            return Err(invalid_input("draft sessions cannot be archived"));
        }
        let counts = self.commit_effects("set_session_archived", &[id.clone(), archived.to_string()],
            vec![StatementSpec::new(
                "UPDATE threads SET archived = ?1 WHERE id = ?2 AND parent_thread_id IS NULL AND frozen_context IS NOT NULL",
                vec![Value::Integer(i64::from(archived)), Value::Text(id.clone())],
            )], id).await?;
        match counts.first() {
            None | Some(1) => Ok(()),
            Some(0) => Err(invalid_input("session is not an archivable root")),
            _ => Err(super::session_codec::corrupt(
                "archive updated multiple sessions",
            )),
        }
    }

    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.read_children(parent).await
    }

    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.read_tree(root).await
    }

    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        self.write_history_append(id, payloads).await
    }

    async fn append_reminder_if_absent(
        &self,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        self.write_reminder_if_absent(id, message_id, reminder)
            .await
    }

    async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.write_close_intent(id).await
    }

    async fn is_session_closing(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.read_close_intent(id).await
    }

    async fn save_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()> {
        self.write_fork(fork).await
    }
    async fn save_fork_in_workspace(
        &self,
        fork: &ForkSnapshot,
        workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        self.write_fork_in_workspace(fork, workspace).await
    }

    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()> {
        self.write_child(child).await
    }

    async fn load_child_resume_record(
        &self,
        child: &ThreadId,
    ) -> SessionResourceResult<ChildResumeRecord> {
        self.read_child_resume(child).await
    }

    async fn store_child_resume_record(
        &self,
        child: &ThreadId,
        record: &ChildResumeRecord,
    ) -> SessionResourceResult<()> {
        self.write_child_resume(child, record).await
    }

    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        self.write_compaction_change(id, change).await
    }

    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()> {
        self.write_projections(id, updates).await
    }

    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
        clear_excluded: &[MessageId],
    ) -> SessionResourceResult<RewindOutcome> {
        self.write_rewind(id, boundary, clear_excluded).await
    }

    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.write_history_removal(id, ids).await
    }

    async fn update_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.write_meta(id, patch).await
    }

    async fn set_work_state(&self, id: &ThreadId, state: WorkState) -> SessionResourceResult<()> {
        self.write_work_state(id, state).await
    }

    async fn settle_work_state_after_turn(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.settle_work_state_after_turn_mq(id).await
    }

    async fn set_context_usage(
        &self,
        id: &ThreadId,
        tokens: Option<u64>,
    ) -> SessionResourceResult<()> {
        self.write_context_usage(id, tokens).await
    }

    async fn delete_tree(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.write_tree_deletion(id).await
    }

    async fn mq_enqueue(
        &self,
        thread_id: &ThreadId,
        input: peri_acp_types::session::MqInput,
    ) -> SessionResourceResult<peri_acp_types::session::MqEnqueueOutcome> {
        self.enqueue_mq(thread_id, input).await
    }

    async fn mq_query(
        &self,
        thread_id: &ThreadId,
        filter: peri_acp_types::session::MqFilter,
    ) -> SessionResourceResult<Vec<peri_acp_types::session::MqEntry>> {
        self.query_mq(thread_id, filter).await
    }

    async fn mq_cancel(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<peri_acp_types::session::MqCancelOutcome> {
        self.cancel_mq(thread_id, id).await
    }

    async fn mq_receive(
        &self,
        thread_id: &ThreadId,
        selection: peri_acp_types::session::MqReceiveSelection,
    ) -> SessionResourceResult<peri_acp_types::session::MqReceiveOutcome> {
        self.receive_mq(thread_id, selection).await
    }

    async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
        self.clear_mq(thread_id).await
    }

    async fn mq_history_contains(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<bool> {
        self.history_contains(thread_id, id).await
    }

    /// 远端 child 终态结算入口；单批语义与终态守卫见 `session_settlement`。
    async fn finalize_child_terminal(
        &self,
        child: &ThreadId,
        status: peri_acp_types::thread::AgentStatus,
        work_state: WorkState,
        target: &ThreadId,
        message: peri_acp_types::session::MqInput,
    ) -> SessionResourceResult<peri_acp_types::session::MqEnqueueOutcome> {
        self.finalize_child_terminal_settlement(child, status, work_state, target, message)
            .await
    }

    /// 远端无结果终态结算入口；守卫与批后分类见 `session_settlement`。
    async fn settle_terminal_without_result(
        &self,
        child: &ThreadId,
        status: peri_acp_types::thread::AgentStatus,
        work_state: WorkState,
    ) -> SessionResourceResult<peri_acp_types::session_resources::TerminalSettlementOutcome> {
        self.settle_terminal_without_result_settlement(child, status, work_state)
            .await
    }

    /// 远端未决收敛：拿本机日志里的操作 id 向远端账本求证终态（C §5.1 第 4 步）。
    ///
    /// 每条未结清记录只有三种结论，且都由**写确认过的证据**给出，不靠空查询或超时推断：
    ///
    /// - 账本行已生效 → 结清 `applied`（原操作生效了，不是丢失）；
    /// - 账本行已封闭 → 结清 `never_applied`；
    /// - 账本行不存在 → 用**同一 id** 做终态封闭竞争：封闭先提交则原请求此后不可能再生效
    ///   （结清 `never_applied`）；竞争发现对方已提交则读回收据（结清 `applied`）；
    ///   封闭本身未确认则保持未结清。
    ///
    /// 账本行存在但不可解释时保持未结清：无法证明就不是收敛。全部结清后才看本机还有没有
    /// 别的未决事实，两者都为空才报告 `Recovered`。
    ///
    /// **封闭与提交互斥**由唯一键给出，不由时序给出：封闭写的是与资格写**同一个主键**，
    /// 本机**没有**远端操作的日志（v10 删除了 `session_remote_operations`，用户裁决不做
    /// 跨安装能力），因此没有「按本机记录里的原操作 id 逐条向远端求证终态」这条路径可走。
    ///
    /// 本方法只回答本机能回答的那部分：本机已没有可证明未结态的 durable 记录，会话数据
    /// 仍可读即可重载。**影响面**：进程崩溃前发出的远端请求若结果未知，本机无法再判定它
    /// 是否生效——这是被撤销的能力，不是遗漏；未结清只在当前实例的 persistence gate 上表达，进程退出后
    /// 不由新的运行句柄宣称前次未知请求已结清。
    ///
    /// 若将来重新引入跨进程未决判定，移除条件是：本机重新持有「发送前登记、确定终态才
    /// 结清」的记录，并且远端账本的终态封闭仍按同一唯一键空间竞争。
    async fn recover_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        // 只读事实：会话数据不存在时不能宣告「已收敛、可重载」。
        self.load_meta(id).await?;
        Ok(PersistenceRecovery::Recovered)
    }

    /// 远端没有异步写入队列，本机也没有未结清记录可供等待，因此没有可排空的东西。
    ///
    /// 在途请求的等待由门面通过当前实例的 persistence gate 完成，adapter 自己不做时序假设。
    async fn drain(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        Ok(())
    }

    /// 关闭连接：把服务中的连接移进关闭句柄，再真正关闭；之后任何调用都明确失败。
    ///
    /// 幂等只适用于**确认关闭**：只有真实关闭成功返回过，后续调用才是幂等成功。关闭一开始
    /// （无论成功与否）连接就离开业务路径且不再重连；失败或取消都不丢句柄——重试关闭的是
    /// **同一条被保留的连接**，不新建连接，也不把「没有连接可用」当成「已经干净关闭」。
    ///
    /// 「确认」的范围是**本机传输面关闭成功**（见
    /// [`RemoteStore::close`](crate::sessions::remote::mutation::RemoteStore::close) 与 `remote` 模块
    /// 文档的 shutdown 定义）：它不证明服务端连接已释放，也不证明任何未知的远端写没有生效——
    /// v10 撤销本机操作日志后，本机已没有可以向远端账本求证的 durable 锚点，这条判定只剩下
    /// 当前实例 persistence gate 的未结清标记；重开不构成对前次未知写入效果的证明。
    async fn close(&self) -> SessionResourceResult<()> {
        // 唯一的翻转点：取走服务中的连接（已经在关闭中时复用同一个句柄）。
        let closing = { self.slot.write().await.begin_close() };
        // 从来没有过连接（关闭态装配）：不谎报成功，也不建连接。
        let Some(closing) = closing else {
            return Err(connection_closed());
        };
        // 真实关闭：成功才确认（进度记在句柄里，不在这个 future 里）；失败如实上报，可重试。
        closing.shutdown().await
    }
}
