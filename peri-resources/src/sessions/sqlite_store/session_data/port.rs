//! `SessionDataPort` 的 SQLite 接线；数据面主体文件只保留类型、模块装配与测试挂载。

use super::*;

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
        self.writable()?;
        let snapshot_at = input
            .meta
            .snapshot_at_message_id
            .map(|id| id.as_uuid().to_string());
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        let resolved = SqliteSessionDatabase::validate_binding_relation_on(&mut tx, &input.binding)
            .await
            .map_err(super::super::failure::binding_relation_failure)?;
        let mut row = new_session_row(
            input,
            snapshot_at.as_deref(),
            Some(input.frozen.as_str()),
            0,
        );
        row.cwd = super::super::discovery::path_text(&resolved.cwd).map_err(write_failure)?;
        insert_thread_row(&mut tx, &row)
            .await
            .map_err(write_failure)?;
        insert_binding_row(&mut tx, &input.thread_id, &input.binding)
            .await
            .map_err(write_failure)?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(input.thread_id.clone())))?;
        Ok(())
    }

    async fn save_new_session_draft(&self, draft: &NewSessionDraft) -> SessionResourceResult<()> {
        self.writable()?;
        let snapshot_at = draft
            .meta
            .snapshot_at_message_id
            .map(|id| id.as_uuid().to_string());
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        let resolved = SqliteSessionDatabase::validate_binding_relation_on(&mut tx, &draft.binding)
            .await
            .map_err(super::super::failure::binding_relation_failure)?;
        let mut row = new_session_draft_row(draft, snapshot_at.as_deref(), 0);
        row.cwd = super::super::discovery::path_text(&resolved.cwd).map_err(write_failure)?;
        insert_thread_row(&mut tx, &row)
            .await
            .map_err(write_failure)?;
        insert_binding_row(&mut tx, &draft.thread_id, &draft.binding)
            .await
            .map_err(write_failure)?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(draft.thread_id.clone())))?;
        Ok(())
    }

    async fn commit_frozen(
        &self,
        id: &ThreadId,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let updated = sqlx::query(
            "UPDATE threads SET frozen_context = ?1 WHERE id = ?2 AND frozen_context IS NULL
             AND EXISTS (SELECT 1 FROM session_bindings WHERE thread_id = ?2)",
        )
        .bind(frozen.as_str())
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(SessionResourceError::conflict(
                "session is not a bound draft with an uncommitted frozen snapshot",
            ));
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
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
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let row: Option<(String, Option<String>, String)> =
            sqlx::query_as("SELECT cwd, parent_thread_id, workspace_id FROM threads WHERE id = ?1")
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        let Some((cwd, parent, owner_workspace_id)) = row else {
            return Err(not_found());
        };
        // 保存的绝对 cwd 是接纳依据；调用方不能借接纳顺手改绑。
        if cwd != saved_cwd || owner_workspace_id != workspace.workspace_id.to_string() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionBindingMismatch),
            ));
        }
        let binding = SessionBinding::from_workspace(workspace);
        // 子会话不另立归属：绑定身份逐字继承父会话（`save_child` 的同一不变量，
        // 父子 cwd 一致也在调用方成立）。接纳因此只在父会话的绑定行已经成立、
        // 且与本绑定一致时放行——父会话还没被本机核实过时如实拒绝，不代父接纳。
        if let Some(parent) = parent.as_deref() {
            let parent_id = ThreadId::from(parent.to_owned());
            match binding_row_state_on(&mut tx, &parent_id).await? {
                BindingRowState::Bound(parent_binding) if parent_binding == binding => {}
                _ => {
                    return Err(SessionResourceError::new(
                        SessionResourceErrorKind::Workspace(
                            WorkspaceError::ExecutionBindingMismatch,
                        ),
                    ))
                }
            }
        }
        // 展示投影（无观测快照，项目 id 由会话 id 派生）不是可接纳的执行证据：接受它
        // 只会在下一句的项目关系比对里失败，并把「没有绑定」误报成「绑定数据不支持」。
        if workspace.discovery_snapshot.is_none() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Workspace(WorkspaceError::UnverifiedLegacyOwner),
            ));
        }
        sqlx::query(
            "INSERT OR IGNORE INTO projects(id, locator, object_identity) VALUES (?1, ?2, ?1)",
        )
        .bind(binding.project_id.to_string())
        .bind(workspace.root.to_string_lossy().into_owned())
        .execute(&mut *tx)
        .await
        .map_err(|error| write_failure(error.into()))?;
        // The workspace owner was established by resolve_workspace.
        // Adoption may attach its immutable evidence, but must not fabricate a
        // workspace owner or change the session's logical workspace.
        let owner_exists: Option<(String,)> =
            sqlx::query_as("SELECT id FROM workspaces WHERE id = ?1 AND project_id = ?2")
                .bind(binding.workspace_id.to_string())
                .bind(binding.project_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        if owner_exists.is_none() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Workspace(WorkspaceError::InvalidBinding),
            ));
        }
        match binding_row_state_on(&mut tx, id).await? {
            BindingRowState::Bound(existing) => {
                // 竞争：已有绑定不覆盖、不修复，必须就是本 workspace。
                if existing != binding {
                    return Err(SessionResourceError::new(
                        SessionResourceErrorKind::Workspace(
                            WorkspaceError::ExecutionBindingMismatch,
                        ),
                    ));
                }
            }
            BindingRowState::Absent => {
                // 缺绑定就是接纳的全部前提：frozen 列已有字节（迁移前已认领、绑定
                // 记录缺失的历史会话）同样是 legacy 现场，字节本身是唯一事实源——
                // COALESCE 只补空列，已有字节不因候选而改变（write-once）。
                sqlx::query(
                    "UPDATE threads SET frozen_context = COALESCE(frozen_context, ?1) WHERE id = ?2",
                )
                .bind(frozen.as_str())
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
                insert_binding_row(&mut tx, id, &binding)
                    .await
                    .map_err(write_failure)?;
            }
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
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
            .map_err(|error| map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let meta = context::load_meta_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        let parent = meta.parent_thread_id.clone();
        let binding = match binding_row_state_on(&mut tx, id).await? {
            BindingRowState::Bound(binding) => BindingState::Bound(binding),
            BindingRowState::Absent if parent.is_some() => BindingState::ExternalOrUnregistered,
            BindingRowState::Absent => BindingState::Missing,
        };
        let frozen = match frozen_bytes_on(&mut tx, id).await.map_err(read_failure)? {
            Some(bytes) => FrozenState::Present(FrozenSnapshotBytes::new(bytes)),
            None => FrozenState::LegacyAbsent,
        };
        let payloads = context::load_payloads_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        let flags = compaction::load_flags_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        let inherited = context::load_inherited_context_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        // 只读事务：没有写入效果可以证明，提交失败仍按原因分类，不判 `Unknown`。
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
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
            .map_err(|error| map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let meta = context::load_meta_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        let parent = meta.parent_thread_id.clone();
        let binding = match binding_row_state_on(&mut tx, id).await? {
            BindingRowState::Bound(binding) => BindingState::Bound(binding),
            BindingRowState::Absent if parent.is_some() => BindingState::ExternalOrUnregistered,
            BindingRowState::Absent => BindingState::Missing,
        };
        let frozen = match frozen_bytes_on(&mut tx, id).await.map_err(read_failure)? {
            Some(bytes) => FrozenState::Present(FrozenSnapshotBytes::new(bytes)),
            None => FrozenState::LegacyAbsent,
        };
        let payloads = context::load_active_payloads_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        let flags = compaction::load_active_flags_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        let inherited = context::load_inherited_context_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
        Ok(ActiveSessionSnapshot {
            meta,
            binding,
            frozen,
            inherited,
            payloads,
            flags,
        })
    }

    async fn load_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState> {
        // 与 `load_snapshot` 同一条分类规则、同一次读取视图，只是不读历史与 frozen。
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let meta = context::load_meta_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        let state = match binding_row_state_on(&mut tx, id).await? {
            BindingRowState::Bound(binding) => BindingState::Bound(binding),
            BindingRowState::Absent if meta.parent_thread_id.is_some() => {
                BindingState::ExternalOrUnregistered
            }
            BindingRowState::Absent => BindingState::Missing,
        };
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
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
            .map_err(|error| map_sqlx(&error))?;
        let mut tx = sqlx::Connection::begin(&mut *connection)
            .await
            .map_err(|error| map_sqlx(&error))?;
        let payloads = context::load_context_payloads_on(&mut tx, id)
            .await
            .map_err(read_failure)?;
        tx.commit().await.map_err(|error| map_sqlx(&error))?;
        Ok(payloads)
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

    async fn load_message_flags(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<HashMap<MessageId, MessageFlags>> {
        compaction::load_flags_for_ids(&self.database, id, ids)
            .await
            .map_err(read_failure)
    }

    async fn load_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        self.database.load_meta(id).await.map_err(read_failure)
    }

    async fn session_exists(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM threads WHERE id = ?1")
            .bind(id.as_str())
            .fetch_optional(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        Ok(row.is_some())
    }

    async fn binding_of(&self, id: &ThreadId) -> SessionResourceResult<Option<SessionBinding>> {
        self.database
            .load_session_binding_impl(id)
            .await
            .map_err(read_failure)
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
                .map_err(|error| map_sqlx(&error))?;
        Ok(row.and_then(|(snapshot,)| snapshot))
    }

    async fn session_root(&self, id: &ThreadId) -> SessionResourceResult<ThreadId> {
        let mut connection = self
            .database
            .pool
            .acquire()
            .await
            .map_err(|error| map_sqlx(&error))?;
        Ok(thread_root_on(&mut connection, id)
            .await
            .unwrap_or_else(|_| id.clone()))
    }

    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.database
            .list_scoped_threads_impl(query)
            .await
            .map_err(write_failure)
    }

    async fn list_archived_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.database
            .list_scoped_threads_by_archive_impl(query, true)
            .await
            .map_err(read_failure)
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
            .map_err(read_failure)
    }

    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.database
            .list_session_threads(root)
            .await
            .map_err(read_failure)
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
        self.writable()?;
        if fork.target.thread_id == fork.source_id {
            return Err(invalid_input("fork target must differ from its source"));
        }
        let snapshot_at = fork
            .target
            .meta
            .snapshot_at_message_id
            .map(|id| id.as_uuid().to_string());
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, &fork.source_id).await?;
        if !thread_exists_on(&mut tx, &fork.source_id)
            .await
            .map_err(read_failure)?
        {
            return Err(not_found());
        }
        insert_thread_row(
            &mut tx,
            &new_session_row(
                &fork.target,
                snapshot_at.as_deref(),
                Some(fork.target.frozen.as_str()),
                fork.payloads.len() as i64,
            ),
        )
        .await
        .map_err(write_failure)?;
        insert_binding_row(&mut tx, &fork.target.thread_id, &fork.target.binding)
            .await
            .map_err(write_failure)?;
        let payload_ids = insert_history_rows(&mut tx, &fork.target.thread_id, &fork.payloads)
            .await
            .map_err(write_failure)?;
        for (message_id, flags) in &fork.flags {
            if !payload_ids.contains(message_id) {
                return Err(invalid_input("fork flags reference an unknown message id"));
            }
            let updated = sqlx::query(
                "UPDATE messages SET truncated = ?1, excluded = ?2, projection = ?3
                 WHERE message_id = ?4 AND thread_id = ?5",
            )
            .bind(flags.truncated)
            .bind(flags.excluded)
            .bind(projection_json(flags)?)
            .bind(message_id.as_uuid().to_string())
            .bind(fork.target.thread_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
            if updated.rows_affected() != 1 {
                return Err(corrupt("fork projection did not apply to exactly one row"));
            }
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(fork.target.thread_id.clone())))?;
        Ok(())
    }

    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()> {
        self.writable()?;
        // 防御校验复用门面同一条规则：落库写的是 `target.meta` 里的父关系，声明与它不一致
        // 时必须在这里就停下，不能靠「父链核对」兜底——那条链查的是声明里的 parent。
        ensure_child_relation(child)?;
        let snapshot_at = child
            .target
            .meta
            .snapshot_at_message_id
            .map(|id| id.as_uuid().to_string());
        let inherited = child
            .inherited
            .to_json()
            .map_err(|_| corrupt("inherited context is not serializable"))?;
        // 发布前校验引用边界，损坏的继承区不能落库。
        InheritedContext::from_json(&inherited)
            .map_err(|_| corrupt("inherited context has invalid message references"))?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        self.require_session(&mut tx, &child.parent_id).await?;
        if !thread_exists_on(&mut tx, &child.parent_id)
            .await
            .map_err(read_failure)?
        {
            return Err(not_found());
        }
        // 父子关系与根归属必须与库内事实一致：child 的 root 必须真是 parent 链的根。
        let parent_root = thread_root_on(&mut tx, &child.parent_id)
            .await
            .map_err(read_failure)?;
        if parent_root != child.root_id {
            return Err(invalid_input("child root does not match its parent chain"));
        }
        // frozen 必须逐字节来自 root 的已保存快照：不重新扫描目录，也不重新冻结。
        let root_frozen = frozen_bytes_on(&mut tx, &child.root_id)
            .await
            .map_err(read_failure)?;
        if root_frozen.as_deref() != Some(child.target.frozen.as_str()) {
            return Err(invalid_input(
                "child frozen snapshot must be the root's saved snapshot",
            ));
        }
        // 子会话继承父会话的执行绑定身份，不另立 workspace 归属。
        match binding_row_state_on(&mut tx, &child.parent_id).await? {
            BindingRowState::Bound(parent_binding) if parent_binding != child.target.binding => {
                return Err(SessionResourceError::new(
                    SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionBindingMismatch),
                ));
            }
            _ => {}
        }
        insert_thread_row(
            &mut tx,
            &new_session_row(
                &child.target,
                snapshot_at.as_deref(),
                Some(child.target.frozen.as_str()),
                0,
            ),
        )
        .await
        .map_err(write_failure)?;
        insert_binding_row(&mut tx, &child.target.thread_id, &child.target.binding)
            .await
            .map_err(write_failure)?;
        sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
            .bind(&inherited)
            .bind(child.target.thread_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(child.target.thread_id.clone())))?;
        Ok(())
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
                .map_err(|error| map_sqlx(&error))?;
        let Some((status,)) = row else {
            return Err(not_found());
        };
        let status: AgentStatus = status
            .parse()
            .map_err(|_| corrupt("stored agent status is not readable"))?;
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
            .map_err(|e| map_sqlx(&e))?;
        self.require_session(&mut tx, child).await?;
        let updated =
            sqlx::query("UPDATE threads SET agent_status = ?1, updated_at = ?2 WHERE id = ?3")
                .bind(record.status.as_str())
                .bind(&now)
                .bind(child.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(child.clone())))?;
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
            .map_err(|e| map_sqlx(&e))?;
        self.require_session(&mut tx, id).await?;
        compaction::commit_compaction_lifecycle_on(&mut tx, id, change)
            .await
            .map_err(write_failure)?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
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
            .map_err(|error| map_sqlx(&error))?;
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
            .map_err(|error| map_sqlx(&error))?;
            if updated.rows_affected() != 1 {
                // 不属于本会话或不存在：既不静默跳过，也不把别会话的行改掉。
                return Err(invalid_input(
                    "projection target is not a history entry of this session",
                ));
            }
        }
        sqlx::query("UPDATE threads SET updated_at = ?1 WHERE id = ?2")
            .bind(peri_time::now_utc_rfc3339())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
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

    async fn close(&self) -> SessionResourceResult<()> {
        // 连接池由共享库句柄所有（执行面也可能在用），这里只关闭数据侧的写入入口。
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}
