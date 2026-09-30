use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    BindingState, ChildSnapshot, ForkSnapshot, FrozenSnapshotBytes, FrozenState, NewSession,
    NewSessionDraft, PersistenceRecovery, RewindBoundary, SessionMetaPatch, SessionResourceError,
    SessionResourceErrorKind, SessionResourceResult, SessionSnapshot,
};
use peri_acp_types::store::{
    serialize_persisted_payload, CompactionChange, InheritedContext, MessageFlags, PersistedPayload,
};
use peri_acp_types::thread::{AgentStatus, ThreadId, ThreadMeta};
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding, WorkspaceError,
    SESSION_BINDING_VERSION,
};
use sqlx::SqliteConnection;

use super::database::SqliteSessionDatabase;
use super::failure::{
    commit_failure, corrupt, invalid_input, map_sqlx, not_found, read_failure, unavailable,
    write_failure,
};
use super::row_mapping::extract_title;
use super::session_rows::{
    delete_thread_child_rows, insert_binding_row, insert_thread_row, ThreadRowInsert,
};
use super::{compaction, context, session_rows, workspace as workspace_store};
use crate::sessions::canonical::payload_role;
use crate::sessions::data::ensure_child_relation;
use crate::sessions::data::ChildResumeRecord;
use crate::sessions::data::SessionDataPort;
use crate::sessions::local_port::SessionFacts;

/// 同一份 [`SqliteSessionDatabase`] 的数据面句柄。
///
/// `closed` 是共享的：门面发给 child resume 认领 handle 的副本与门面自身看到同一个
/// 关闭状态，关闭一个即关闭全部写入入口。
#[derive(Clone)]
pub(crate) struct SqliteSessionData {
    database: Arc<SqliteSessionDatabase>,
    /// 端口关闭后不再接受新写入；读取不受影响（历史仍可解释）。
    closed: Arc<AtomicBool>,
}

impl SqliteSessionData {
    pub(super) fn new(database: Arc<SqliteSessionDatabase>) -> Self {
        Self {
            database,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 写入前置：端口未关闭且本次打开可写。
    fn writable(&self) -> SessionResourceResult<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(unavailable("session data port is closed"));
        }
        if self.database.is_read_only() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::ReadOnlyStore,
            ));
        }
        Ok(())
    }

    /// 未发布创建的撤销：`require_uncommitted` 决定是否叠加「未提交 frozen」判据。
    ///
    /// 两条入口共用同一份删除顺序（子会话守卫 → 执行代际 → 子表 → threads 行），只有
    /// 判据按调用方语义分档：
    ///
    /// - `false`：write-once 完整创建（fork 等）的失败补偿——目标创建即带 frozen，
    ///   撤销就是把它整条删掉（可由 source 重生成）；
    /// - `true`：两阶段草稿（`SessionInitialization::abandon`）——已定稿的草稿是
    ///   「已提交、未发布」的合法中间态，必须拒绝删除（typed 冲突），走 dirty 恢复。
    async fn revoke_created_row(
        &self,
        id: &ThreadId,
        require_uncommitted: bool,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        // 撤销只针对「本次未发布的创建」：已经派生过子会话的 identity 不能被补偿掉，
        // 否则子会话会指向一个不存在的父节点。
        let children: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM threads WHERE parent_thread_id = ?1")
                .bind(id.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        if children.0 > 0 {
            return Err(invalid_input(
                "session has published children and cannot be revoked",
            ));
        }
        if require_uncommitted {
            // 两阶段草稿的判据：已定稿 frozen 的会话不是未发布草稿，删除它会销毁内容
            // 准入的成果（它该走 dirty 恢复，而不是被补偿掉）。
            let committed: Option<(Option<String>,)> =
                sqlx::query_as("SELECT frozen_context FROM threads WHERE id = ?1")
                    .bind(id.as_str())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            if matches!(committed, Some((Some(_),))) {
                return Err(SessionResourceError::conflict(
                    "session has a committed frozen snapshot and cannot be revoked",
                ));
            }
        }
        // 撤销即撤销：数据行与执行代际在同一次提交里消失，本机不再留「这个 identity 的
        // 初始化被刻意放弃」的终态锚点（那张表随 v10 删除）。identity 的复用判定因此只
        // 依据现有数据事实，不再有第二份本机证据。
        //
        // 本机执行代际不靠外键级联（v7 起 execution_runs 无外键），显式删除。
        sqlx::query("DELETE FROM execution_runs WHERE thread_id = ?1")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        // 子表行同样显式删除，不借 `ON DELETE CASCADE`：那份级联只在 SQLite 上存在，
        // 远端执行器没有（见 [`session_rows::THREAD_CHILD_DELETES`]）。先子后父。
        delete_thread_child_rows(&mut tx, id.as_str())
            .await
            .map_err(|error| map_sqlx(&error))?;
        sqlx::query(session_rows::DELETE_THREAD_SQL)
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }
}

// ─── 行读取原语 ───────────────────────────────────────────────────────────────

#[path = "session_data_helpers.rs"]
mod helpers;
use helpers::*;
pub(super) use helpers::{new_session_draft_row, new_session_row};

#[async_trait]
impl SessionDataPort for SqliteSessionData {
    fn oauth_credentials(
        self: Arc<Self>,
    ) -> Option<Arc<dyn peri_acp_types::oauth_credentials::OAuthCredentialPort>> {
        Some(Arc::new(
            super::oauth_credentials::SqliteOAuthCredentialStore::new(self.database.clone()),
        ))
    }

    async fn machine_id_of(&self, id: &ThreadId) -> SessionResourceResult<Option<String>> {
        machine_id_on(&self.database.pool, id).await
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
        insert_thread_row(
            &mut tx,
            &new_session_row(
                input,
                snapshot_at.as_deref(),
                Some(input.frozen.as_str()),
                0,
            ),
        )
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
        insert_thread_row(
            &mut tx,
            &new_session_draft_row(draft, snapshot_at.as_deref(), 0),
        )
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

    /// 本机组合不经过这条路径：frozen 的提交必须与「本进程仍是活 owner」「代际未结清」
    /// 在同一事务内成立（[`super::local::LocalExecution::commit_frozen`]）。这里如实报告
    /// 不支持，而不是提供一个缺少 owner 校验的第二条写入路径。
    async fn commit_frozen(
        &self,
        _id: &ThreadId,
        _frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ))
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
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT cwd, parent_thread_id FROM threads WHERE id = ?1")
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        let Some((cwd, parent)) = row else {
            return Err(not_found());
        };
        // 保存的绝对 cwd 是接纳依据；调用方不能借接纳顺手改绑或接纳 child。
        if cwd != saved_cwd || parent.is_some() {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionBindingMismatch),
            ));
        }
        let binding = SessionBinding::from_workspace(workspace);
        sqlx::query(
            "INSERT OR IGNORE INTO projects(id, locator, object_identity) VALUES (?1, ?2, ?1)",
        )
        .bind(binding.project_id.to_string())
        .bind(workspace.root.to_string_lossy().into_owned())
        .execute(&mut *tx)
        .await
        .map_err(|error| write_failure(error.into()))?;
        sqlx::query("INSERT OR IGNORE INTO workspaces(id, project_id, root, root_identity, discovery) VALUES (?1, ?2, ?3, ?1, 'null')")
        .bind(binding.workspace_id.to_string()).bind(binding.project_id.to_string()).bind(workspace.root.to_string_lossy().into_owned())
            .execute(&mut *tx).await.map_err(|error| write_failure(error.into()))?;
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
        // `cached_context` 正文）：派生缓存不是历史事实，历史由 payloads 给出。
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

    async fn session_root(&self, id: &ThreadId) -> SessionResourceResult<ThreadId> {
        // 父链解析不出（会话行缺失、链有环）时退回自身：执行代际的键因此仍然确定，
        // 只是阻塞范围变窄，不影响调用方对「这条会话自己」的判定。
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
        self.writable()?;
        if payloads.is_empty() {
            return Ok(());
        }
        // 相同 ID 的碰撞不能静默忽略：批次内重复与库存重复都必须失败，
        // 否则「已存在但内容不同」会被当成成功。
        let mut seen = HashSet::with_capacity(payloads.len());
        for payload in payloads {
            if !seen.insert(payload.id()) {
                return Err(invalid_input("history batch repeats a message id"));
            }
        }
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        if !thread_exists_on(&mut tx, id).await.map_err(read_failure)? {
            // 目标会话不存在与「外键拒绝」是两个不同的原因，前者更可诊断。
            return Err(not_found());
        }
        for payload in payloads {
            sqlx::query(
                "INSERT INTO messages (message_id, thread_id, role, content)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(payload.id().as_uuid().to_string())
            .bind(id.as_str())
            .bind(payload_role(payload))
            .bind(
                serialize_persisted_payload(payload)
                    .map_err(|_| corrupt("history entry is not serializable"))?,
            )
            .execute(&mut *tx)
            .await
            .map_err(|error| map_sqlx(&error))?;
        }
        let now = Utc::now().to_rfc3339();
        let updated = sqlx::query(
            "UPDATE threads SET updated_at = ?1,
                message_count = (SELECT COUNT(*) FROM messages WHERE thread_id = ?2)
             WHERE id = ?2",
        )
        .bind(&now)
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        let messages = payloads
            .iter()
            .filter_map(PersistedPayload::as_message)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(title) = extract_title(&messages) {
            sqlx::query("UPDATE threads SET title = ?1 WHERE id = ?2 AND title IS NULL")
                .bind(&title)
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
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
        let now = Utc::now().to_rfc3339();
        let updated =
            sqlx::query("UPDATE threads SET agent_status = ?1, updated_at = ?2 WHERE id = ?3")
                .bind(record.status.as_str())
                .bind(&now)
                .bind(child.as_str())
                .execute(&self.database.pool)
                .await
                .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        Ok(())
    }

    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        compaction::commit_compaction_lifecycle(&self.database, id, change)
            .await
            .map_err(write_failure)
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
        // 派生视图与写入同事务失效：调用方不需要再补一次 cache 维护。
        sqlx::query(
            "UPDATE threads SET cached_context = NULL, context_cache_epoch = context_cache_epoch + 1
             WHERE id = ?1",
        )
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
    ) -> SessionResourceResult<()> {
        self.writable()?;
        let target = boundary.message_id().as_uuid().to_string();
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        let rowid: Option<(i64,)> =
            sqlx::query_as("SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2")
                .bind(id.as_str())
                .bind(&target)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        // 未知截止点保持无变更语义：找不到目标就不动历史。
        if let Some((rowid,)) = rowid {
            let sql = match boundary {
                RewindBoundary::KeepThrough(_) => {
                    "DELETE FROM messages WHERE thread_id = ?1 AND rowid > ?2"
                }
                RewindBoundary::RemoveFrom(_) => {
                    "DELETE FROM messages WHERE thread_id = ?1 AND rowid >= ?2"
                }
            };
            sqlx::query(sql)
                .bind(id.as_str())
                .bind(rowid)
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
            refresh_history_derivations(&mut tx, id).await?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }

    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if ids.is_empty() {
            return Ok(());
        }
        let unique: Vec<MessageId> = {
            let mut seen = HashSet::with_capacity(ids.len());
            ids.iter().copied().filter(|id| seen.insert(*id)).collect()
        };
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        for message_id in &unique {
            // 别会话的条目不允许被「精确移除」静默命中或静默跳过。
            let owner: Option<(String,)> =
                sqlx::query_as("SELECT thread_id FROM messages WHERE message_id = ?1")
                    .bind(message_id.as_uuid().to_string())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| map_sqlx(&error))?;
            match owner {
                Some((owner,)) if owner == id.as_str() => {
                    sqlx::query("DELETE FROM messages WHERE message_id = ?1 AND thread_id = ?2")
                        .bind(message_id.as_uuid().to_string())
                        .bind(id.as_str())
                        .execute(&mut *tx)
                        .await
                        .map_err(|error| map_sqlx(&error))?;
                }
                Some(_) => return Err(invalid_input("history entry belongs to another session")),
                // 已经不存在的条目是幂等删除，不产生错误。
                None => {}
            }
        }
        refresh_history_derivations(&mut tx, id).await?;
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }

    async fn update_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if patch.title.is_none()
            && patch.status.is_none()
            && patch.cancel_policy.is_none()
            && patch.config.is_none()
        {
            // 没有字段要改：不写、也不假装写入了新时间戳。
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let mut builder: sqlx::QueryBuilder<sqlx::Sqlite> =
            sqlx::QueryBuilder::new("UPDATE threads SET updated_at = ");
        builder.push_bind(&now);
        if let Some(title) = &patch.title {
            builder.push(", title = ").push_bind(title.clone());
        }
        if let Some(status) = &patch.status {
            builder.push(", agent_status = ").push_bind(status.as_str());
        }
        if let Some(policy) = &patch.cancel_policy {
            builder
                .push(", cancel_policy = ")
                .push_bind(policy.as_str());
        }
        if let Some(config) = &patch.config {
            builder.push(", config = ").push_bind(config.clone());
        }
        builder.push(" WHERE id = ").push_bind(id.as_str());
        let updated = builder
            .build()
            .execute(&self.database.pool)
            .await
            .map_err(|error| map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        Ok(())
    }

    async fn delete_tree(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.writable()?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| map_sqlx(&error))?;
        if !thread_exists_on(&mut tx, id).await.map_err(read_failure)? {
            return Err(not_found());
        }
        let tree = thread_tree_on(&mut tx, id).await.map_err(read_failure)?;
        // 删除即删除：数据行与执行代际在同一次提交里消失。v10 之前这里还会写一条删除
        // 墓碑（`session_lifecycle_commitments`），那是本机为「这条 identity 被刻意终止」
        // 留的第二份证据；用户裁决撤销跨安装的终态判定后，墓碑连同表一起删除。
        //
        // v7 起 execution_runs 不再有外键：不显式删除就会留下永不收敛的孤儿执行行。
        for thread in &tree {
            sqlx::query("DELETE FROM execution_runs WHERE thread_id = ?1")
                .bind(thread.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        }
        // 子表行显式删除，不借 `ON DELETE CASCADE`：级联只在 SQLite 上存在，远端执行器
        // 没有（见 [`session_rows::THREAD_CHILD_DELETES`]）。顺序与远端 delete_tree 一致：
        // 子行全部先删，最后才删 threads 行。
        for thread in &tree {
            delete_thread_child_rows(&mut tx, thread)
                .await
                .map_err(|error| map_sqlx(&error))?;
        }
        for thread in &tree {
            sqlx::query(session_rows::DELETE_THREAD_SQL)
                .bind(thread)
                .execute(&mut *tx)
                .await
                .map_err(|error| map_sqlx(&error))?;
        }
        tx.commit()
            .await
            .map_err(|_| commit_failure(Some(id.clone())))?;
        Ok(())
    }

    async fn recover_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        // 本机写入与执行代际同事务完成，**没有**跨进程的未决记录可收敛（v10 删除了
        // `session_lifecycle_commitments` 与 `session_remote_operations`）。这里只复核
        // 数据事实仍然可读：读不到就如实失败，不用「已收敛」把不存在的会话说成可重载。
        self.load_meta(id).await?;
        Ok(PersistenceRecovery::Recovered)
    }

    async fn drain(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        // 本机写入是同事务完成的，没有排队中的持久化，也没有本机未决记录可等：本方法
        // 因此不阻塞。在途写入的等待由门面按活跃租约完成（见 `drain_persistence`）。
        Ok(())
    }

    async fn close(&self) -> SessionResourceResult<()> {
        // 连接池由共享库句柄所有（执行面也可能在用），这里只关闭数据侧的写入入口。
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}
