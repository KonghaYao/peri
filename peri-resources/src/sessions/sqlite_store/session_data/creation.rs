//! Atomic creation, draft publication, legacy adoption, fork, and child writes.

use super::workspace_store;
use super::ChildSnapshot;
use super::ForkSnapshot;
use super::FrozenSnapshotBytes;
use super::InheritedContext;
use super::NewSession;
use super::NewSessionDraft;
use super::ResolvedWorkspace;
use super::SessionBinding;
use super::SessionResourceError;
use super::SessionResourceErrorKind;
use super::SessionResourceResult;
use super::SqliteSessionData;
use super::SqliteSessionDatabase;
use super::ThreadId;
use super::WorkspaceError;
use crate::sessions::sqlite_store::session_data::binding_row_state_on;
use crate::sessions::sqlite_store::session_data::frozen_bytes_on;
use crate::sessions::sqlite_store::session_data::insert_history_rows;
use crate::sessions::sqlite_store::session_data::projection_json;
use crate::sessions::sqlite_store::session_data::thread_exists_on;
use crate::sessions::sqlite_store::session_data::thread_root_on;
use crate::sessions::sqlite_store::session_data::BindingRowState;
impl SqliteSessionData {
    pub(super) async fn save_new_session_transaction(
        &self,
        input: &NewSession,
    ) -> SessionResourceResult<()> {
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
            .map_err(|error| super::map_sqlx(&error))?;
        let resolved = SqliteSessionDatabase::validate_binding_relation_on(&mut tx, &input.binding)
            .await
            .map_err(super::super::failure::binding_relation_failure)?;
        let mut row = super::new_session_row(
            input,
            snapshot_at.as_deref(),
            Some(input.frozen.as_str()),
            0,
        );
        row.cwd =
            super::super::discovery::path_text(&resolved.cwd).map_err(super::write_failure)?;
        super::insert_thread_row(&mut tx, &row)
            .await
            .map_err(super::write_failure)?;
        super::insert_binding_row(&mut tx, &input.thread_id, &input.binding)
            .await
            .map_err(super::write_failure)?;
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(input.thread_id.clone())))?;
        Ok(())
    }

    pub(super) async fn save_new_session_draft_transaction(
        &self,
        draft: &NewSessionDraft,
    ) -> SessionResourceResult<()> {
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
            .map_err(|error| super::map_sqlx(&error))?;
        let resolved = SqliteSessionDatabase::validate_binding_relation_on(&mut tx, &draft.binding)
            .await
            .map_err(super::super::failure::binding_relation_failure)?;
        let mut row = super::new_session_draft_row(draft, snapshot_at.as_deref(), 0);
        row.cwd =
            super::super::discovery::path_text(&resolved.cwd).map_err(super::write_failure)?;
        super::insert_thread_row(&mut tx, &row)
            .await
            .map_err(super::write_failure)?;
        super::insert_binding_row(&mut tx, &draft.thread_id, &draft.binding)
            .await
            .map_err(super::write_failure)?;
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(draft.thread_id.clone())))?;
        Ok(())
    }

    pub(super) async fn commit_frozen_transaction(
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
            .map_err(|error| super::map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let updated = sqlx::query(
            "UPDATE threads SET frozen_context = ?1 WHERE id = ?2 AND frozen_context IS NULL
             AND EXISTS (SELECT 1 FROM session_bindings WHERE thread_id = ?2)",
        )
        .bind(frozen.as_str())
        .bind(id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|error| super::map_sqlx(&error))?;
        if updated.rows_affected() != 1 {
            return Err(SessionResourceError::conflict(
                "session is not a bound draft with an uncommitted frozen snapshot",
            ));
        }
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(id.clone())))?;
        Ok(())
    }

    pub(super) async fn adopt_legacy_session_transaction(
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
            .map_err(|error| super::map_sqlx(&error))?;
        self.require_session(&mut tx, id).await?;
        let row: Option<(String, Option<String>, String)> =
            sqlx::query_as("SELECT cwd, parent_thread_id, workspace_id FROM threads WHERE id = ?1")
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| super::map_sqlx(&error))?;
        let Some((cwd, parent, owner_workspace_id)) = row else {
            return Err(super::not_found());
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
                    ));
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
        // 接纳不制造项目：workspace 归属行由 resolve_workspace 建立，项目必须已登记。
        workspace_store::require_project_exists(&mut tx, binding.project_id)
            .await
            .map_err(super::write_failure)?;
        // The workspace owner was established by resolve_workspace.
        // Adoption may attach its immutable evidence, but must not fabricate a
        // workspace owner or change the session's logical workspace.
        let owner_exists: Option<(String,)> =
            sqlx::query_as("SELECT id FROM workspaces WHERE id = ?1 AND project_id = ?2")
                .bind(binding.workspace_id.to_string())
                .bind(binding.project_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| super::map_sqlx(&error))?;
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
                .map_err(|error| super::map_sqlx(&error))?;
                super::insert_binding_row(&mut tx, id, &binding)
                    .await
                    .map_err(super::write_failure)?;
            }
        }
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(id.clone())))?;
        Ok(())
    }

    pub(super) async fn save_fork_transaction(
        &self,
        fork: &ForkSnapshot,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        if fork.target.thread_id == fork.source_id {
            return Err(super::invalid_input(
                "fork target must differ from its source",
            ));
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
            .map_err(|error| super::map_sqlx(&error))?;
        self.require_session(&mut tx, &fork.source_id).await?;
        if !thread_exists_on(&mut tx, &fork.source_id)
            .await
            .map_err(super::read_failure)?
        {
            return Err(super::not_found());
        }
        super::insert_thread_row(
            &mut tx,
            &super::new_session_row(
                &fork.target,
                snapshot_at.as_deref(),
                Some(fork.target.frozen.as_str()),
                fork.payloads.len() as i64,
            ),
        )
        .await
        .map_err(super::write_failure)?;
        super::insert_binding_row(&mut tx, &fork.target.thread_id, &fork.target.binding)
            .await
            .map_err(super::write_failure)?;
        let payload_ids = insert_history_rows(&mut tx, &fork.target.thread_id, &fork.payloads)
            .await
            .map_err(super::write_failure)?;
        for (message_id, flags) in &fork.flags {
            if !payload_ids.contains(message_id) {
                return Err(super::invalid_input(
                    "fork flags reference an unknown message id",
                ));
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
            .map_err(|error| super::map_sqlx(&error))?;
            if updated.rows_affected() != 1 {
                return Err(super::corrupt(
                    "fork projection did not apply to exactly one row",
                ));
            }
        }
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(fork.target.thread_id.clone())))?;
        Ok(())
    }

    pub(super) async fn save_child_transaction(
        &self,
        child: &ChildSnapshot,
    ) -> SessionResourceResult<()> {
        self.writable()?;
        // 防御校验复用门面同一条规则：落库写的是 `target.meta` 里的父关系，声明与它不一致
        // 时必须在这里就停下，不能靠「父链核对」兜底——那条链查的是声明里的 parent。
        super::ensure_child_relation(child)?;
        let snapshot_at = child
            .target
            .meta
            .snapshot_at_message_id
            .map(|id| id.as_uuid().to_string());
        let inherited = child
            .inherited
            .to_json_with_spans()
            .map_err(|_| super::corrupt("inherited context is not serializable"))?;
        // 发布前校验引用边界，损坏的继承区不能落库。
        InheritedContext::from_json(&inherited.json)
            .map_err(|_| super::corrupt("inherited context has invalid message references"))?;
        let mut tx = self
            .database
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        self.require_session(&mut tx, &child.parent_id).await?;
        if !thread_exists_on(&mut tx, &child.parent_id)
            .await
            .map_err(super::read_failure)?
        {
            return Err(super::not_found());
        }
        // 父子关系与根归属必须与库内事实一致：child 的 root 必须真是 parent 链的根。
        let parent_root = thread_root_on(&mut tx, &child.parent_id)
            .await
            .map_err(super::read_failure)?;
        if parent_root != child.root_id {
            return Err(super::invalid_input(
                "child root does not match its parent chain",
            ));
        }
        // frozen 必须逐字节来自 root 的已保存快照：不重新扫描目录，也不重新冻结。
        let root_frozen = frozen_bytes_on(&mut tx, &child.root_id)
            .await
            .map_err(super::read_failure)?;
        if root_frozen.as_deref() != Some(child.target.frozen.as_str()) {
            return Err(super::invalid_input(
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
        super::insert_thread_row(
            &mut tx,
            &super::new_session_row(
                &child.target,
                snapshot_at.as_deref(),
                Some(child.target.frozen.as_str()),
                0,
            ),
        )
        .await
        .map_err(super::write_failure)?;
        super::insert_binding_row(&mut tx, &child.target.thread_id, &child.target.binding)
            .await
            .map_err(super::write_failure)?;
        sqlx::query("UPDATE threads SET inherited_context = ?1 WHERE id = ?2")
            .bind(&inherited.json)
            .bind(child.target.thread_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| super::map_sqlx(&error))?;
        // `inherited_context` 字节是唯一事实源：v15 起不再派生索引（可从本列重建）。
        tx.commit()
            .await
            .map_err(|_| super::commit_failure(Some(child.target.thread_id.clone())))?;
        Ok(())
    }
}
