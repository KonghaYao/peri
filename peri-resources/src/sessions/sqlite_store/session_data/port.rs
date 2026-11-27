//! `SessionDataPort` 的 SQLite 接线；数据面主体文件只保留类型、模块装配与测试挂载。

use super::{
    async_trait, compaction, context, session_rows, ActiveSessionSnapshot, AgentStatus, Arc,
    BindingState, ChildResumeRecord, ChildSnapshot, CloseSettlement, CompactionChange,
    ForkSnapshot, FrozenSnapshotBytes, FrozenState, HashMap, HistoryCursor, HistoryForwardCursor,
    HistoryPage, HistoryPageBounds, HistorySegment, MessageFlags, MessageId, MqCancelOutcome,
    MqEnqueueOutcome, MqEntry, MqFilter, MqInput, MqReceiveOutcome, MqReceiveSelection, NewSession,
    NewSessionDraft, Ordering, PersistedPayload, PersistenceRecovery, ResolvedWorkspace,
    RewindBoundary, RewindCandidate, RewindOutcome, ScopedThreadPage, ScopedThreadQuery,
    SessionBinding, SessionDataPort, SessionMetaPatch, SessionResourceResult, SessionSnapshot,
    SqliteSessionData, TerminalSettlementOutcome, ThreadId, ThreadMeta, TrustedSystemReminder,
    WorkState,
};
use crate::sessions::sqlite_store::session_data::binding_row_state_on;
use crate::sessions::sqlite_store::session_data::frozen_bytes_on;
use crate::sessions::sqlite_store::session_data::frozen_state_on;
use crate::sessions::sqlite_store::session_data::machine_id_on;
use crate::sessions::sqlite_store::session_data::projection_json;
use crate::sessions::sqlite_store::session_data::recovery_scope_statement_with;
use crate::sessions::sqlite_store::session_data::strict_thread_root_on;
use crate::sessions::sqlite_store::session_data::thread_root_on;
use crate::sessions::sqlite_store::session_data::BindingRowState;
use crate::sessions::sqlite_store::session_data::ThreadRootFailure;
use peri_acp_types::session_resources::history::TranscriptRestoreState;

use crate::sessions::sqlite_store::row_mapping::THREAD_META_COLUMNS;

#[async_trait]
impl SessionDataPort for SqliteSessionData {
    fn oauth_credentials_for_workspace(
        self: Arc<Self>,
        workspace_id: peri_acp_types::workspace::WorkspaceId,
    ) -> Option<Arc<dyn peri_acp_types::oauth_credentials::OAuthCredentialPort>> {
        Some(Arc::new(
            super::super::oauth_credentials::SqliteOAuthCredentialStore::new(
                self.database.clone(),
                workspace_id,
            ),
        ))
    }

    async fn finish_close(&self, root: &ThreadId) -> SessionResourceResult<()> {
        self.finish_close_intent(root).await
    }

    async fn close_settlement(&self, root: &ThreadId) -> SessionResourceResult<CloseSettlement> {
        self.read_close_settlement(root).await
    }

    async fn machine_id_of(&self, id: &ThreadId) -> SessionResourceResult<Option<String>> {
        machine_id_on(&self.database.pool, id).await
    }
    async fn workspace_id_of(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<peri_acp_types::workspace::WorkspaceId>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT workspace_id FROM threads WHERE id = ?1")
                .bind(id)
                .fetch_optional(&self.database.pool)
                .await
                .map_err(|error| super::super::failure::read_failure(error.into()))?;
        row.map(|(id,)| {
            id.parse()
                .map_err(|_| super::super::failure::corrupt("stored workspace id is invalid"))
        })
        .transpose()
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
        self.save_new_session_transaction(input).await
    }

    async fn save_new_session_draft(&self, draft: &NewSessionDraft) -> SessionResourceResult<()> {
        self.save_new_session_draft_transaction(draft).await
    }

    async fn commit_frozen(
        &self,
        id: &ThreadId,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.commit_frozen_transaction(id, frozen).await
    }

    async fn revoke_unpublished_session(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.revoke_created_row(id, false).await
    }

    async fn revoke_unpublished_draft(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.revoke_created_row(id, true).await
    }

    async fn adopt_legacy_session(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.adopt_legacy_session_transaction(id, saved_cwd, workspace, frozen)
            .await
    }

    async fn load_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot> {
        // 一次读取视图：同一连接上的延迟事务让 meta/binding/frozen/历史/继承区来自
        // 同一个数据库状态，不让调用方拼多次跨时刻查询。meta 走轻量投影（不含
        // 正文）：历史事实由 payloads 给出。
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        let meta = context::load_meta_on(&mut tx, id, THREAD_META_COLUMNS)
            .await
            .map_err(super::read_failure)?;
        let parent = meta.parent_thread_id.clone();
        let binding = match binding_row_state_on(&mut tx, id).await? {
            BindingRowState::Bound(binding) => BindingState::Bound(binding),
            BindingRowState::Absent if parent.is_some() => BindingState::ExternalOrUnregistered,
            BindingRowState::Absent => BindingState::Missing,
        };
        let frozen = match frozen_bytes_on(&mut tx, id)
            .await
            .map_err(super::read_failure)?
        {
            Some(bytes) => FrozenState::Present(FrozenSnapshotBytes::new(bytes)),
            None => FrozenState::LegacyAbsent,
        };
        let payloads = context::load_payloads_on(&mut tx, id)
            .await
            .map_err(super::read_failure)?;
        let flags = compaction::load_flags_on(&mut tx, id)
            .await
            .map_err(super::read_failure)?;
        let inherited = context::load_inherited_context_on(&mut tx, id, THREAD_META_COLUMNS)
            .await
            .map_err(super::read_failure)?;
        // 只读事务：没有写入效果可以证明，提交失败仍按原因分类，不判 `Unknown`。
        tx.commit().await.map_err(|error| super::map_sqlx(&error))?;
        Ok(SessionSnapshot {
            meta,
            binding,
            frozen,
            payloads,
            flags,
            inherited,
        })
    }

    async fn load_active_snapshot(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<ActiveSessionSnapshot> {
        // 与 `load_snapshot` 同一次读取视图、同一套分类规则，差别只有范围：
        // payload 与 flags 的读取条件都带 `excluded = 0`，已排除正文与已排除条目的
        // 标记都不进入内存（不先全量再过滤）。
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        let meta = context::load_meta_on(&mut tx, id, THREAD_META_COLUMNS)
            .await
            .map_err(super::read_failure)?;
        let parent = meta.parent_thread_id.clone();
        let binding = match binding_row_state_on(&mut tx, id).await? {
            BindingRowState::Bound(binding) => BindingState::Bound(binding),
            BindingRowState::Absent if parent.is_some() => BindingState::ExternalOrUnregistered,
            BindingRowState::Absent => BindingState::Missing,
        };
        let frozen = match frozen_bytes_on(&mut tx, id)
            .await
            .map_err(super::read_failure)?
        {
            Some(bytes) => FrozenState::Present(FrozenSnapshotBytes::new(bytes)),
            None => FrozenState::LegacyAbsent,
        };
        let payloads = context::load_active_payloads_on(&mut tx, id)
            .await
            .map_err(super::read_failure)?;
        let state = context::load_transcript_restore_state_on(&mut tx, id, THREAD_META_COLUMNS)
            .await
            .map_err(super::read_failure)?;
        tx.commit().await.map_err(|error| super::map_sqlx(&error))?;
        Ok(ActiveSessionSnapshot {
            meta,
            binding,
            frozen,
            inherited: state.inherited,
            payloads,
            flags: state.own_flags,
        })
    }

    async fn load_transcript_restore_state(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<TranscriptRestoreState> {
        let mut tx = self
            .database
            .pool
            .begin()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let state = context::load_transcript_restore_state_on(&mut tx, id, THREAD_META_COLUMNS)
            .await
            .map_err(super::read_failure)?;
        tx.commit().await.map_err(|error| super::map_sqlx(&error))?;
        Ok(state)
    }

    async fn load_frozen_state(&self, id: &ThreadId) -> SessionResourceResult<FrozenState> {
        // 单列读取：恢复准入只需要 frozen 原文，不触碰 payload/flags/inherited。
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        frozen_state_on(&mut connection, id).await
    }
    fn supports_history_window(&self) -> bool {
        true
    }
    fn supports_history_v2(&self) -> bool {
        true
    }

    async fn load_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState> {
        // 与 `load_snapshot` 同一条分类规则、同一次读取视图，只是不读历史与 frozen。
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        let state = match binding_row_state_on(&mut tx, id).await? {
            BindingRowState::Bound(binding) => BindingState::Bound(binding),
            BindingRowState::Absent => {
                let parent: Option<String> =
                    sqlx::query_scalar("SELECT parent_thread_id FROM threads WHERE id = ?1")
                        .bind(id.as_str())
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(|e| super::map_sqlx(&e))?;
                if parent.is_some() {
                    BindingState::ExternalOrUnregistered
                } else {
                    BindingState::Missing
                }
            }
        };
        tx.commit().await.map_err(|error| super::map_sqlx(&error))?;
        Ok(state)
    }

    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        let payloads = context::load_context_payloads_on(&mut tx, id, THREAD_META_COLUMNS)
            .await
            .map_err(super::read_failure)?;
        tx.commit().await.map_err(|error| super::map_sqlx(&error))?;
        Ok(payloads)
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

    async fn initial_history_anchor(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<(HistoryPageBounds, Option<MessageId>)> {
        self.read_initial_history_anchor(id).await
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

    async fn history_window_bounds(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::session_resources::SessionHistoryWindowBounds> {
        self.read_history_window_bounds(id).await
    }

    async fn load_history_window_page(
        &self,
        id: &ThreadId,
        plan: &peri_acp_types::session_resources::HistoryWindowPagePlan,
    ) -> SessionResourceResult<peri_acp_types::session_resources::HistoryWindowRows> {
        self.read_history_window_page(id, plan).await
    }

    async fn seek_history_window(
        &self,
        id: &ThreadId,
        source: peri_acp_types::history_window::HistorySource,
        anchor: MessageId,
        before_limit: usize,
        after_limit: usize,
        byte_budget: usize,
        committed_tail: &peri_acp_types::history_window::HistoryCommittedTail,
    ) -> SessionResourceResult<peri_acp_types::session_resources::HistoryWindowRows> {
        self.read_history_window_seek(
            id,
            source,
            anchor,
            before_limit,
            after_limit,
            byte_budget,
            committed_tail,
        )
        .await
    }

    async fn read_history_entry_metadata(
        &self,
        id: &ThreadId,
        source: peri_acp_types::history_common::HistorySource,
        entry_id: MessageId,
        committed_tail: Option<&peri_acp_types::history_common::HistoryCommittedTail>,
        read_ticket: &peri_acp_types::history_common::HistoryReadTicket,
    ) -> SessionResourceResult<peri_acp_types::session_resources::HistoryEntryMetadata> {
        self.read_history_entry_metadata_from_store(
            id,
            source,
            entry_id,
            committed_tail,
            read_ticket,
        )
        .await
    }

    fn own_history_body_reader(
        &self,
    ) -> Option<&dyn peri_acp_types::session_resources::OwnHistoryBodyReader> {
        Some(self)
    }

    async fn load_history_body_chunk(
        &self,
        id: &ThreadId,
        source: peri_acp_types::history_window::HistorySource,
        entry_id: MessageId,
        offset: u64,
        max_bytes: u32,
        committed_tail: Option<&peri_acp_types::history_window::HistoryCommittedTail>,
        read_ticket: &peri_acp_types::history_window::HistoryReadTicket,
    ) -> SessionResourceResult<peri_acp_types::history_window::HistoryBodyChunk> {
        self.read_history_body_chunk(
            id,
            source,
            entry_id,
            offset,
            max_bytes,
            committed_tail,
            read_ticket,
        )
        .await
    }

    async fn load_entry_positions(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<HashMap<MessageId, u64>> {
        self.read_active_entry_positions(id).await
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

    async fn load_message_flags(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<HashMap<MessageId, MessageFlags>> {
        compaction::load_flags_for_ids(&self.database, id, ids)
            .await
            .map_err(super::read_failure)
    }

    async fn load_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        self.database
            .load_meta(id)
            .await
            .map_err(super::read_failure)
    }

    async fn session_exists(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM threads WHERE id = ?1")
            .bind(id.as_str())
            .fetch_optional(&self.database.pool)
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        Ok(row.is_some())
    }

    async fn binding_of(&self, id: &ThreadId) -> SessionResourceResult<Option<SessionBinding>> {
        self.database
            .load_session_binding_impl(id)
            .await
            .map_err(super::read_failure)
    }

    async fn binding_discovery_snapshot(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Option<String>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT discovery_snapshot FROM session_bindings WHERE thread_id = ?1")
                .bind(id.as_str())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(|error| super::map_sqlx(&error))?;
        Ok(row.and_then(|(snapshot,)| snapshot))
    }

    async fn session_root(&self, id: &ThreadId) -> SessionResourceResult<ThreadId> {
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        Ok(thread_root_on(&mut connection, id)
            .await
            .unwrap_or_else(|_| id.clone()))
    }

    async fn recovery_scope(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<peri_acp_types::recovery::RecoveryScope> {
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        // 根必须严格解析：缺失或成环时范围不可解释，退回自身会把未知当已知。
        let root = match strict_thread_root_on(&mut connection, id).await {
            Ok(root) => root,
            Err(ThreadRootFailure::Missing) => {
                return Err(super::corrupt("thread ancestry is missing"))
            }
            Err(ThreadRootFailure::Cyclic) => return Err(super::corrupt("cyclic thread ancestry")),
            Err(ThreadRootFailure::Store(error)) => return Err(super::read_failure(error)),
        };
        let (projection, filter) = self.database.recovery_scope_sql_parts();
        let rows: Vec<(String, Option<String>, Option<i64>, String)> = sqlx::query_as(
            sqlx::AssertSqlSafe(recovery_scope_statement_with(projection, filter)),
        )
        .bind(root.as_str())
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| super::map_sqlx(&error))?;
        let mut candidates = Vec::with_capacity(rows.len());
        for (thread_id, parent_thread_id, usage, state) in rows {
            let state = state
                .parse::<peri_acp_types::thread::WorkState>()
                .map_err(|_| super::corrupt("stored work state is not a known value"))?;
            // SQL 已按 working 过滤；这里只防御未来投影漂移，不静默改成候选。
            if state != peri_acp_types::thread::WorkState::Working {
                continue;
            }
            let context_usage_tokens = match usage {
                None => None,
                Some(usage) => Some(
                    u64::try_from(usage)
                        .map_err(|_| super::corrupt("stored context usage is negative"))?,
                ),
            };
            candidates.push(peri_acp_types::recovery::RecoveryCandidate {
                thread_id,
                parent_thread_id,
                context_usage_tokens,
            });
        }
        Ok(peri_acp_types::recovery::RecoveryScope {
            root_thread_id: root,
            candidates,
        })
    }

    async fn interrupted_threads(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<peri_acp_types::recovery::InterruptedThread>> {
        self.interrupted_threads_impl(id).await
    }

    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.database
            .list_scoped_threads_impl(query)
            .await
            .map_err(super::write_failure)
    }

    async fn list_archived_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.database
            .list_scoped_threads_by_archive_impl(query, true)
            .await
            .map_err(super::read_failure)
    }

    async fn set_session_archived(
        &self,
        id: &ThreadId,
        archived: bool,
    ) -> SessionResourceResult<()> {
        self.catalog_set_session_archived(id, archived).await
    }

    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.database
            .list_child_threads(parent)
            .await
            .map_err(super::read_failure)
    }

    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.database
            .list_session_threads(root)
            .await
            .map_err(super::read_failure)
    }

    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        self.append_history_rows(id, payloads).await
    }

    async fn append_reminder_if_absent(
        &self,
        id: &ThreadId,
        message_id: MessageId,
        reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        self.append_terminal_reminder(id, message_id, reminder)
            .await
    }

    async fn mark_session_closing(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.write_close_intent(id).await
    }

    async fn is_session_closing(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.read_close_intent(id).await
    }

    async fn save_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()> {
        self.save_fork_transaction(fork).await
    }

    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()> {
        self.save_child_transaction(child).await
    }

    async fn load_child_resume_record(
        &self,
        child: &ThreadId,
    ) -> SessionResourceResult<ChildResumeRecord> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT agent_status FROM threads WHERE id = ?1")
                .bind(child.as_str())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(|error| super::map_sqlx(&error))?;
        let Some((status,)) = row else {
            return Err(super::not_found());
        };
        let status: AgentStatus = status
            .parse()
            .map_err(|_| super::corrupt("stored agent status is not readable"))?;
        Ok(ChildResumeRecord {
            // 认领事实就是「该会话正在运行」：active 之外的状态都可被认领。
            claimed: status.is_active(),
            status,
        })
    }

    async fn store_child_resume_record(
        &self,
        child: &ThreadId,
        record: &ChildResumeRecord,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let now = peri_time::now_utc_rfc3339();
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| super::map_sqlx(&e))?;
        self.require_session(&mut tx, child).await?;
        let updated =
            sqlx::query("UPDATE threads SET agent_status = ?1, updated_at = ?2 WHERE id = ?3")
                .bind(record.status.as_str())
                .bind(&now)
                .bind(child.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| super::map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(super::not_found());
        }
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(child.clone())))?;
        Ok(())
    }

    async fn settle_child_resume(
        &self,
        child: &ThreadId,
        previous: &ChildResumeRecord,
        work_state: Option<WorkState>,
        terminal: Option<AgentStatus>,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let now = peri_time::now_utc_rfc3339();
        let status = terminal.unwrap_or(previous.status);
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| super::map_sqlx(&e))?;
        self.require_session(&mut tx, child).await?;
        // 两列必须在同一条语句里落地：分开提交会留下「状态已回滚、工作意图未回滚」
        // 的中间态，之后的补偿会因状态不再属于本次认领而失去重试责任。
        let updated = sqlx::query(
            "UPDATE threads SET agent_status = ?1, updated_at = ?2,
             work_state = COALESCE(?3, work_state) WHERE id = ?4",
        )
        .bind(status.as_str())
        .bind(&now)
        .bind(work_state.map(|state| state.as_str()))
        .bind(child.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| super::map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(super::not_found());
        }
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(child.clone())))?;
        Ok(())
    }

    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|e| super::map_sqlx(&e))?;
        self.require_session(&mut tx, id).await?;
        compaction::commit_compaction_lifecycle_on(&mut tx, id, change)
            .await
            .map_err(super::write_failure)?;
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(id.clone())))?;
        Ok(())
    }

    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if updates.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        for (message_id, flags) in updates {
            let updated = sqlx::query(
                "UPDATE messages SET truncated = ?1, excluded = ?2, projection = ?3
                 WHERE message_id = ?4 AND thread_id = ?5",
            )
            .bind(flags.truncated)
            .bind(flags.excluded)
            .bind(projection_json(flags)?)
            .bind(message_id.as_uuid().to_string())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| super::map_sqlx(&error))?;
            if updated.rows_affected() != 1 {
                // 不属于本会话或不存在：既不静默跳过，也不把别会话的行改掉。
                return Err(super::invalid_input(
                    "projection target is not a history entry of this session",
                ));
            }
        }
        sqlx::query("UPDATE threads SET updated_at = ?1 WHERE id = ?2")
            .bind(peri_time::now_utc_rfc3339())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(id.clone())))?;
        Ok(())
    }

    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
        clear_excluded: &[MessageId],
    ) -> SessionResourceResult<RewindOutcome> {
        self.rewind_stored_history(id, boundary, clear_excluded)
            .await
    }

    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.remove_stored_history_entries(id, ids).await
    }

    async fn update_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.update_stored_meta(id, patch).await
    }

    async fn set_work_state(&self, id: &ThreadId, state: WorkState) -> SessionResourceResult<()> {
        // 单列写入与迁移桥共用同一份 SQL：不碰 `updated_at`，不改运行态。
        session_rows::write_work_state(&self.database.pool, id, state)
            .await
            .map_err(super::write_failure)
    }

    async fn settle_work_state_after_turn(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.settle_work_state_after_turn_mq(id).await
    }

    async fn set_context_usage(
        &self,
        id: &ThreadId,
        tokens: Option<u64>,
    ) -> SessionResourceResult<()> {
        session_rows::write_context_usage(&self.database.pool, id, tokens)
            .await
            .map_err(super::write_failure)
    }

    async fn delete_tree(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.delete_stored_tree(id).await
    }

    async fn recover_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        self.load_meta(id).await?;
        Ok(PersistenceRecovery::Recovered)
    }

    async fn drain(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        Ok(())
    }

    async fn mq_enqueue(
        &self,
        thread_id: &ThreadId,
        input: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        self.enqueue_mq(thread_id, input).await
    }

    async fn mq_query(
        &self,
        thread_id: &ThreadId,
        filter: MqFilter,
    ) -> SessionResourceResult<Vec<MqEntry>> {
        self.query_mq(thread_id, filter).await
    }

    async fn mq_headers(
        &self,
        thread_id: &ThreadId,
        filter: MqFilter,
    ) -> SessionResourceResult<Vec<peri_acp_types::session::MqHeader>> {
        self.query_mq_headers(thread_id, filter).await
    }

    async fn mq_count(&self, thread_id: &ThreadId) -> SessionResourceResult<usize> {
        self.count_mq(thread_id).await
    }

    async fn mq_exists(&self, thread_id: &ThreadId) -> SessionResourceResult<bool> {
        self.exists_mq(thread_id).await
    }

    async fn mq_watermark(
        &self,
        thread_id: &ThreadId,
    ) -> SessionResourceResult<Option<peri_acp_types::session::MqWatermark>> {
        self.watermark_mq(thread_id).await
    }

    async fn mq_cancel(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<MqCancelOutcome> {
        self.cancel_mq(thread_id, id).await
    }

    async fn mq_receive(
        &self,
        thread_id: &ThreadId,
        selection: MqReceiveSelection,
    ) -> SessionResourceResult<MqReceiveOutcome> {
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
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        Ok(
            super::mq::message_fact(&mut connection, &id.as_uuid().to_string())
                .await?
                .is_some_and(|(owner, _)| owner == thread_id.as_str()),
        )
    }

    async fn finalize_child_terminal(
        &self,
        child: &ThreadId,
        status: AgentStatus,
        work_state: WorkState,
        target: &ThreadId,
        message: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        self.finalize_child_terminal_mq(child, status, work_state, target, message)
            .await
    }

    async fn settle_terminal_without_result(
        &self,
        child: &ThreadId,
        status: AgentStatus,
        work_state: WorkState,
    ) -> SessionResourceResult<TerminalSettlementOutcome> {
        self.settle_terminal_without_result_rows(child, status, work_state)
            .await
    }

    async fn settle_retained_mq(
        &self,
        thread_id: &ThreadId,
        reminders: &[peri_acp_types::session::RetainedReminder],
        rows: &[MqEntry],
    ) -> SessionResourceResult<()> {
        self.settle_retained_mq_rows(thread_id, reminders, rows)
            .await
    }

    async fn close(&self) -> SessionResourceResult<()> {
        // 连接池由共享库句柄所有（执行面也可能在用），这里只关闭数据侧的写入入口。
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}
