//! Registry and session binding transactions; SQL-scoped lightweight history pages.

use super::{
    database::SqliteSessionDatabase,
    discovery::{self, Discovery, Observation},
};
use crate::sessions::failure::not_found;
use anyhow::{Context, Result};
use peri_acp_types::{
    thread::{ThreadId, ThreadListEntry, ThreadMeta},
    workspace::*,
};
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};
use std::path::{Component, Path, PathBuf};

pub(super) type BindingRow = (i64, String, String, String);

/// 归属行按 `(machine_id, path)` 命中的已记录证据：`(id, project_id, discovery, identity)`。
///
/// `project_id` 与 `discovery` 为 NULL 表示这行从未被写打开观测过（历史惰性行）；`identity`
/// 是当前占用该路径的文件对象证据，只作诊断，不参与身份或执行准入。只读节点按 NULL 判
/// 「本机不认识这个目录」。
type RecordedWorkspace = (String, Option<String>, Option<String>, Option<String>);

pub(super) fn decode_binding(row: BindingRow) -> Result<SessionBinding> {
    if row.0 != i64::from(SESSION_BINDING_VERSION) {
        return Err(WorkspaceError::InvalidBinding.into());
    }
    let relative = PathBuf::from(row.3);
    validate_relative(&relative)?;
    Ok(SessionBinding {
        schema_version: SESSION_BINDING_VERSION,
        revision: 1,
        project_id: row.1.parse().map_err(|_| WorkspaceError::InvalidBinding)?,
        workspace_id: row.2.parse().map_err(|_| WorkspaceError::InvalidBinding)?,
        cwd_relative_to_workspace: relative,
    })
}

/// 相对路径必须是纯普通分量（不含 `..`、根、前缀），且文本可逆。
///
/// 绑定写入与绑定解码共用本规则：写入方不能存下无法解码的路径。
pub(super) fn validate_relative(path: &Path) -> Result<()> {
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(WorkspaceError::InvalidBinding.into());
    }
    discovery::path_text(path)?;
    Ok(())
}

/// 写绑定前确认项目行存在。
///
/// 项目不参与执行准入（归属行当前项目与绑定项目不同仍可恢复），但**写入**绑定必须
/// 引用已登记项目。缺失按 `InvalidBinding` 拒绝，不静默补造项目行。
pub(super) async fn require_project_exists(
    connection: &mut SqliteConnection,
    project_id: ProjectId,
) -> Result<()> {
    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM projects WHERE id = ?1")
        .bind(project_id.to_string())
        .fetch_optional(&mut *connection)
        .await?;
    if row.is_none() {
        return Err(WorkspaceError::InvalidBinding.into());
    }
    Ok(())
}

/// 本次观测能否作为该归属行记录的执行证据。
///
/// 行上还没有已记录快照时（迁移建出的历史惰性行），本次观测就是它的第一条证据：
/// 没有已记录的 Git 布局可以被这条目录模式观测改写，因此不要求 Git 回答。行上已有
/// 快照时只比较 Git 布局（root/common/private 路径）；文件对象身份随环境变化，不参与
/// 判定。布局不同且本次观测来自 Git 的真实回答时可以改写；Git 不可用的目录模式观测
/// 证明不了原仓库消失了，仍按证据不足拒绝。
fn may_write_observation(recorded: Option<&str>, current: &str, git_answered: bool) -> bool {
    let Some(recorded) = recorded else {
        return true;
    };
    let recorded = serde_json::from_str::<Discovery>(recorded).ok();
    let current = serde_json::from_str::<Discovery>(current).ok();
    // 已记录快照解析失败按「布局不同」处理：只有 Git 回答过才允许覆盖。
    let same_layout = matches!(
        (recorded, current),
        (Some(recorded), Some(current))
            if recorded.root == current.root
                && recorded.common_dir == current.common_dir
                && recorded.private_dir == current.private_dir
    );
    same_layout || git_answered
}

#[test]
fn observation_write_follows_recorded_layout_and_git_answer() {
    let directory = |root: &str| {
        format!(
            r#"{{"root":"{root}","root_identity":{{"device":1,"inode":1}},"common_dir":null,"common_identity":null,"private_dir":null,"private_identity":null}}"#
        )
    };
    let repository = |root: &str| {
        format!(
            r#"{{"root":"{root}","root_identity":{{"device":2,"inode":2}},"common_dir":"{root}/.git","common_identity":{{"device":2,"inode":2}},"private_dir":"{root}/.git","private_identity":{{"device":2,"inode":2}}}}"#
        )
    };
    // 从未观测过的历史归组行接受第一条目录模式观测（迁移期行没有可被改写的布局）。
    assert!(may_write_observation(None, &directory("/work"), false));
    // 行上已有快照：同一 Git 布局照写（inode 变化不影响），不同布局只接受 Git 回答过的观测。
    assert!(may_write_observation(
        Some(&directory("/work")),
        &directory("/work"),
        false
    ));
    // 同一布局、不同 device/inode（容器重启/同路径重建）：允许写。
    let rebuilt = r#"{"root":"/work","root_identity":{"device":7,"inode":7},"common_dir":null,"common_identity":null,"private_dir":null,"private_identity":null}"#;
    assert!(may_write_observation(
        Some(&directory("/work")),
        rebuilt,
        false
    ));
    assert!(may_write_observation(
        Some(&directory("/work")),
        &repository("/work"),
        true
    ));
    assert!(!may_write_observation(
        Some(&directory("/work")),
        &repository("/work"),
        false
    ));
    // 已记录快照解析失败：按布局不同处理，没有 Git 回答时拒绝改写。
    assert!(!may_write_observation(
        Some("broken"),
        &repository("/work"),
        false
    ));
    assert!(may_write_observation(
        Some("broken"),
        &repository("/work"),
        true
    ));
}

impl SqliteSessionDatabase {
    pub(super) async fn resolve_workspace_impl(&self, cwd: &Path) -> Result<ResolvedWorkspace> {
        let (cwd, observed) = discovery::observe(cwd).await?;
        let Observation {
            discovery: discovered,
            git_answered,
        } = observed;
        let root = discovery::path_text(&discovered.root)?;
        let locator = discovery::path_text(discovered.project_locator())?;
        let identity = serde_json::to_string(discovered.project_identity())?;
        let snapshot = serde_json::to_string(&discovered)?;
        let root_identity = serde_json::to_string(&discovered.root_identity)?;
        let source = if discovered.common_dir.is_some() || discovered.private_dir.is_some() {
            "discovered"
        } else {
            "unverified"
        };
        let machine = crate::sessions::machine::current()?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        // 归属键是 `(machine_id, path)`：同机同路径只有一个 Workspace。项目、文件对象
        // 身份与发现快照只是该行记录**当前占用这个路径的对象**，不参与工作区身份，也不
        // 参与执行准入：目录被替换（同路径的新对象）或换位（同一对象的新路径）都不移动
        // 归属，已有绑定只要 cwd 可用就继续执行，不会静默改绑。
        let recorded: Option<RecordedWorkspace> = sqlx::query_as(
            "SELECT id, project_id, discovery, identity FROM workspaces WHERE machine_id = ?1 AND path = ?2",
        )
        .bind(machine)
        .bind(root)
        .fetch_optional(&mut *tx)
        .await?;
        let (workspace_id, project_id) = match recorded {
            Some((id, project, _, _)) if self.read_only => {
                // 只读节点不能补写证据：读请求按这条归属行已记录的事实回答；没有项目
                // 证据说明它从未被写打开观测过，按「本机不认识这个目录」失败。
                let project = project.ok_or(WorkspaceError::InvalidBinding)?;
                (id.parse::<WorkspaceId>()?, project.parse::<ProjectId>()?)
            }
            Some((id, project, recorded, _)) => {
                // 目录模式的变化必须以 Git 的回答为准：只读目录观测（Git 没回答）不能
                // 证明已记录的仓库消失了，保持 fail-closed，而不是把仓库改写成普通目录。
                if !may_write_observation(recorded.as_deref(), &snapshot, git_answered) {
                    return Err(WorkspaceError::NeedsRelink.into());
                }
                // 已有归属行沿用其登记项目：同路径换对象、`git init`、移除 `.git` 都
                // 不改变行上的 project_id（项目不参与执行准入）。canonical locator 只
                // 用于新建归属行或尚未记录项目的历史惰性行选择/复用项目。
                let project_id = match project {
                    Some(project) => project.parse::<ProjectId>()?,
                    None => Self::observation_project(&mut tx, locator, &identity).await?,
                };
                sqlx::query(
                    "UPDATE workspaces SET project_id = ?1, identity = ?2, discovery = ?3,
                     path_source = CASE WHEN path_source = 'unverified' AND ?4 = 'discovered'
                                        THEN 'discovered' ELSE path_source END
                     WHERE id = ?5",
                )
                .bind(project_id.to_string())
                .bind(&root_identity)
                .bind(&snapshot)
                .bind(source)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
                (id.parse::<WorkspaceId>()?, project_id)
            }
            None => {
                // 只读节点不能登记新工作区：读请求按「本节点没有这条归属」失败，而不是
                // 交给 SQLite 在写入时才报只读。
                self.require_writable()?;
                super::workspace_identity::ensure_current_machine(&mut tx).await?;
                let project_id = Self::observation_project(&mut tx, locator, &identity).await?;
                let id = WorkspaceId::new();
                sqlx::query(
                    "INSERT INTO workspaces(id, machine_id, path, path_source, project_id, identity, discovery)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .bind(id.to_string())
                .bind(machine)
                .bind(root)
                .bind(source)
                .bind(project_id.to_string())
                .bind(&root_identity)
                .bind(&snapshot)
                .execute(&mut *tx)
                .await?;
                (id, project_id)
            }
        };
        discovered.reassert_key_objects(&cwd).await?;
        tx.commit().await?;
        let relative_cwd = cwd
            .strip_prefix(&discovered.root)
            .map_err(|_| WorkspaceError::NeedsRelink)?
            .to_path_buf();
        Ok(ResolvedWorkspace {
            project_id,
            workspace_id,
            cwd,
            root: discovered.root,
            relative_cwd,
            discovery_snapshot: Some(snapshot),
        })
    }

    /// 把无绑定会话的历史归属收敛到本机可证实的 Workspace（幂等修复，可重跑）。
    ///
    /// 只允许一种改写：会话没有绑定行，且原归属行**从未被本机观测过**（`project_id`
    /// 与 `discovery` 都为 NULL 的迁移期历史归组行——它的路径只是保存 cwd 的归组，
    /// 不构成执行证据）。行上已有证据说明本机认识那个目录，改写会静默改绑，直接
    /// 拒绝；已有绑定行同理（绑定事实先到，修复入口只按既有事实回答，不覆盖）。
    pub(super) async fn relink_unbound_ownership_impl(
        &self,
        id: &ThreadId,
        workspace: &ResolvedWorkspace,
    ) -> Result<()> {
        self.require_writable()?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row: Option<(Option<String>, String)> =
            sqlx::query_as("SELECT parent_thread_id, workspace_id FROM threads WHERE id = ?1")
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        let Some((parent, owner)) = row else {
            return Err(not_found().into());
        };
        if parent.is_some() {
            // 子会话继承父会话的归属，不按自身保存的 cwd 另建归属。
            return Err(WorkspaceError::UnverifiedLegacyOwner.into());
        }
        let bound: Option<(String,)> =
            sqlx::query_as("SELECT thread_id FROM session_bindings WHERE thread_id = ?1")
                .bind(id.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        if bound.is_some() {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        if owner == workspace.workspace_id.to_string() {
            return Ok(());
        }
        let recorded: Option<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT project_id, discovery FROM workspaces WHERE id = ?1")
                .bind(&owner)
                .fetch_optional(&mut *tx)
                .await?;
        if !matches!(recorded, Some((None, None))) {
            return Err(WorkspaceError::UnverifiedLegacyOwner.into());
        }
        // 目标归属必须就是本次观测写下的那一行：修复只收敛归属，不制造无证据的归属。
        let target: Option<(String,)> =
            sqlx::query_as("SELECT id FROM workspaces WHERE id = ?1 AND project_id = ?2")
                .bind(workspace.workspace_id.to_string())
                .bind(workspace.project_id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        if target.is_none() {
            return Err(WorkspaceError::UnverifiedLegacyOwner.into());
        }
        sqlx::query("UPDATE threads SET workspace_id = ?1 WHERE id = ?2")
            .bind(workspace.workspace_id.to_string())
            .bind(id.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 新归属行（或尚无项目的历史惰性行）的项目记录：按 canonical locator 选择/复用。
    ///
    /// 已有归属行不会走到这里——它沿用行上登记的 `project_id`，`git init`、移除
    /// `.git` 或同路径换文件对象都不改项目。`identity` 只写入 `object_identity` 列
    /// 作为诊断值，不参与匹配。
    async fn observation_project(
        connection: &mut SqliteConnection,
        locator: &str,
        identity: &str,
    ) -> Result<ProjectId> {
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM projects WHERE locator = ?1 ORDER BY rowid ASC LIMIT 1")
                .bind(locator)
                .fetch_optional(&mut *connection)
                .await?;
        match existing {
            Some((id,)) => Ok(id.parse::<ProjectId>()?),
            None => {
                let id = ProjectId::new();
                sqlx::query("INSERT INTO projects (id, locator, object_identity) VALUES (?, ?, ?)")
                    .bind(id.to_string())
                    .bind(locator)
                    .bind(identity)
                    .execute(&mut *connection)
                    .await?;
                Ok(id)
            }
        }
    }

    /// 事务内的复核：归属行与路径结构一致、机器一致，且执行目录可用。
    ///
    /// 文件对象身份与 Git 布局只保留在快照里作诊断，不再参与准入通过/失败判定；
    /// 项目也不是执行准入身份，绑定自身的 `project_id` 与归属行当前项目不一致
    /// 不影响执行。归属行路径必须就是 workspace 的 root，root + relative 必须就是
    /// 执行 cwd；这些是纯路径结构校验，防止自相矛盾的 `ResolvedWorkspace` 写入绑定。
    ///
    /// Transaction callers reuse their admitted connection, including every SQL read.
    async fn validate_resolved_on(
        connection: &mut SqliteConnection,
        workspace: &ResolvedWorkspace,
    ) -> Result<()> {
        validate_relative(&workspace.relative_cwd)?;
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT machine_id, path FROM workspaces WHERE id = ?1")
                .bind(workspace.workspace_id.to_string())
                .fetch_optional(&mut *connection)
                .await?;
        let (machine, path) = row.ok_or(WorkspaceError::InvalidBinding)?;
        if Path::new(&path) != workspace.root
            || binding_cwd(&workspace.root, &workspace.relative_cwd) != workspace.cwd
        {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        if machine != crate::sessions::machine::current()? {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        discovery::ensure_cwd_available(&workspace.cwd).await?;
        Ok(())
    }

    /// 事务外的完整复核：现在与事务内复核等价，只要求执行目录仍可用。
    ///
    /// 保留薄封装以维持 `full` 的调用签名；完整发现快照比对已因 inode 不参与准入而移除。
    async fn revalidate_registered_observation_on(
        _connection: &mut SqliteConnection,
        workspace: &ResolvedWorkspace,
    ) -> Result<()> {
        discovery::ensure_cwd_available(&workspace.cwd)
            .await
            .map(|_| ())
    }

    pub(super) async fn create_bound_thread_impl(
        &self,
        mut meta: ThreadMeta,
        workspace: &ResolvedWorkspace,
    ) -> Result<ThreadId> {
        self.require_writable()?;
        // 父会话的绑定用于继承归属：比较执行归属字段（不含诊断快照）。提交前在写
        // 事务内再复核一次归属关系、路径结构与 cwd 可用性。
        if let Some(parent) = &meta.parent_thread_id {
            let parent_workspace = self.reassert_session_binding_impl(parent).await?;
            if !parent_workspace.same_execution_binding(workspace) {
                return Err(WorkspaceError::ExecutionBindingMismatch.into());
            }
        }
        meta.cwd = discovery::path_text(&workspace.cwd)?.to_owned();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::validate_resolved_on(&mut tx, workspace).await?;
        require_project_exists(&mut tx, workspace.project_id).await?;
        sqlx::query("INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
            parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, agent_status, workspace_id)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&meta.id).bind(&meta.title).bind(&meta.cwd).bind(meta.created_at.to_rfc3339()).bind(meta.updated_at.to_rfc3339())
            .bind(meta.message_count as i64).bind(&meta.parent_thread_id).bind(&meta.snapshot_at_message_id).bind(meta.hidden)
            .bind(meta.cancel_policy.as_str()).bind(&meta.config).bind(meta.agent_status.as_str())
            .bind(workspace.workspace_id.to_string())
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO session_bindings (thread_id, schema_version, project_id, workspace_id, relative_cwd,
            discovery_snapshot, evidence_origin)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'creation_snapshot')")
            .bind(&meta.id).bind(i64::from(SESSION_BINDING_VERSION)).bind(workspace.project_id.to_string())
            .bind(workspace.workspace_id.to_string()).bind(discovery::path_text(&workspace.relative_cwd)?)
            .bind(workspace.discovery_snapshot.as_deref())
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(meta.id)
    }

    pub(super) async fn load_session_binding_impl(
        &self,
        id: &ThreadId,
    ) -> Result<Option<SessionBinding>> {
        let row: Option<BindingRow> = sqlx::query_as("SELECT schema_version, project_id, workspace_id, relative_cwd FROM session_bindings WHERE thread_id = ?")
            .bind(id).fetch_optional(&self.pool).await?;
        row.map(decode_binding).transpose()
    }

    pub(super) async fn adopt_legacy_thread_impl(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen_snapshot: &str,
    ) -> Result<()> {
        if self.read_only {
            return Err(WorkspaceError::ReadOnlyStore.into());
        }
        if !Path::new(saved_cwd).is_absolute() {
            return Err(WorkspaceError::Unavailable.into());
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let (cwd, parent, owner): (String, Option<String>, String) =
            sqlx::query_as("SELECT cwd, parent_thread_id, workspace_id FROM threads WHERE id = ?")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if cwd != saved_cwd || owner != workspace.workspace_id.to_string() {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        let binding = SessionBinding::from_workspace(workspace);
        // 子会话不另立归属：绑定身份逐字继承父会话，父会话的绑定行必须已经成立且与本
        // 绑定一致（与 `SessionDataPort::adopt_legacy_session` 同一规则）。
        if let Some(parent) = parent.as_deref() {
            let parent_id = ThreadId::from(parent.to_owned());
            let parent_binding: Option<BindingRow> = sqlx::query_as(
                "SELECT schema_version, project_id, workspace_id, relative_cwd FROM session_bindings WHERE thread_id = ?",
            )
            .bind(&parent_id)
            .fetch_optional(&mut *tx)
            .await?;
            match parent_binding.map(decode_binding).transpose()? {
                Some(parent_binding) if parent_binding == binding => {}
                _ => return Err(WorkspaceError::ExecutionBindingMismatch.into()),
            }
        }
        let canonical = tokio::fs::canonicalize(&cwd)
            .await
            .map_err(|_| WorkspaceError::Unavailable)?;
        if canonical != workspace.cwd {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        Self::validate_resolved_on(&mut tx, workspace).await?;
        let existing: Option<BindingRow> = sqlx::query_as("SELECT schema_version, project_id, workspace_id, relative_cwd FROM session_bindings WHERE thread_id = ?")
            .bind(id).fetch_optional(&mut *tx).await?;
        if let Some(row) = existing {
            // A concurrent restorer may have won. Never overwrite or repair its binding.
            decode_binding(row)?;
            let existing_workspace = Self::validate_session_binding_on(&mut tx, id).await?;
            if !existing_workspace.same_execution_binding(workspace) {
                return Err(WorkspaceError::ExecutionBindingMismatch.into());
            }
        } else {
            // 缺绑定就是接纳的全部前提：frozen 列已有字节同样是 legacy 现场，字节本身
            // 是唯一事实源——COALESCE 只补空列，已有字节不因候选而改变（write-once）。
            require_project_exists(&mut tx, workspace.project_id).await?;
            sqlx::query(
                "UPDATE threads SET frozen_context = COALESCE(frozen_context, ?) WHERE id = ?",
            )
            .bind(frozen_snapshot)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO session_bindings (thread_id, schema_version, project_id, workspace_id, relative_cwd,
                discovery_snapshot, evidence_origin)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'legacy_last_observation')")
                .bind(id).bind(i64::from(SESSION_BINDING_VERSION))
                .bind(workspace.project_id.to_string()).bind(workspace.workspace_id.to_string())
                .bind(discovery::path_text(&workspace.relative_cwd)?)
                .bind(workspace.discovery_snapshot.as_deref())
                .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 已有绑定的权威复核：关系一致且执行目录仍可用。
    ///
    /// 这是**一次准入的复核动作**：调用方（`session/load`、prompt 轮、`workflow/resume`
    /// 等）以此为本次准入判定的全部依据，一次准入只应调用一次。准入内的后续检查用
    /// `reassert_session_binding_impl`；两者现在等价，都不启动外部进程。
    pub(super) async fn validate_session_binding_impl(
        &self,
        id: &ThreadId,
    ) -> Result<ResolvedWorkspace> {
        let mut connection = self.pool.acquire().await?;
        Self::validate_session_binding_on(&mut connection, id).await
    }

    /// 同一次准入内的复核：与 `validate_session_binding_impl` 同一套判定。
    ///
    /// 文件对象身份与 Git 布局不再是准入证据，因此这里与权威复核没有差别：只读
    /// SQL 关系并要求 cwd 可用，不重新执行 Git 发现。
    pub(super) async fn reassert_session_binding_impl(
        &self,
        id: &ThreadId,
    ) -> Result<ResolvedWorkspace> {
        let mut connection = self.pool.acquire().await?;
        Self::validate_session_binding_on(&mut connection, id).await
    }

    pub(super) async fn validate_session_binding_on(
        connection: &mut SqliteConnection,
        id: &ThreadId,
    ) -> Result<ResolvedWorkspace> {
        let row: Option<BindingRow> = sqlx::query_as("SELECT schema_version, project_id, workspace_id, relative_cwd FROM session_bindings WHERE thread_id = ?")
            .bind(id).fetch_optional(&mut *connection).await?;
        let binding = decode_binding(row.ok_or(WorkspaceError::BindingMissing)?)?;
        let workspace = Self::validate_binding_relation_on(connection, &binding).await?;
        let owner: (String,) = sqlx::query_as("SELECT workspace_id FROM threads WHERE id = ?1")
            .bind(id)
            .fetch_one(&mut *connection)
            .await?;
        if owner.0 != workspace.workspace_id.to_string() {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        discovery::ensure_cwd_available(&workspace.cwd).await?;
        Ok(workspace)
    }

    /// 绑定值的本机复核：归属行关系加执行目录可用性。
    ///
    /// 与 [`Self::validate_session_binding_on`] 的差别只在于事实来源：那个从已保存的
    /// binding 行读，这个复核调用方手上的 binding 值（新建/fork/child 在写入前用它，
    /// 避免未登记的 project/workspace 直接落到外键失败上）。
    ///
    /// 归属身份就是归属行的 `id`（`(machine_id, path)`）：查不到这行，或这行属于别的
    /// 机器，都是「本机不认识这个绑定」，保持 `InvalidBinding`。
    ///
    /// 行还在、机器也对，但绑定记录的项目已经不同于归属行当前项目（同路径换了文件
    /// 对象、Git 布局变化）：项目不是执行准入身份，不据此拒绝执行，也不自动改绑；
    /// `ResolvedWorkspace.project_id` 保留绑定自身的值。只有 cwd 不存在或不是目录时
    /// 失败关闭。
    pub(super) async fn validate_binding_relation_on(
        connection: &mut SqliteConnection,
        binding: &SessionBinding,
    ) -> Result<ResolvedWorkspace> {
        let row: Option<(String, String, Option<String>)> =
            sqlx::query_as("SELECT path, machine_id, discovery FROM workspaces WHERE id = ?1")
                .bind(binding.workspace_id.to_string())
                .fetch_optional(&mut *connection)
                .await?;
        let (path, machine, discovery) = row.ok_or(WorkspaceError::InvalidBinding)?;
        if machine != crate::sessions::machine::current()? {
            return Err(WorkspaceError::InvalidBinding.into());
        }
        let root = PathBuf::from(&path);
        let workspace = ResolvedWorkspace {
            project_id: binding.project_id,
            workspace_id: binding.workspace_id,
            cwd: binding_cwd(&root, &binding.cwd_relative_to_workspace),
            root,
            relative_cwd: binding.cwd_relative_to_workspace.clone(),
            discovery_snapshot: discovery,
        };
        Self::validate_resolved_on(connection, &workspace).await?;
        Ok(workspace)
    }

    /// 绑定**值**的本机复核（远程组合：绑定来自远端会话行，不在本机 `session_bindings`）。
    ///
    /// 判定与本机绑定完全同一套：归属关系（workspace 必须在本机登记过）加执行目录
    /// 可用性。`full` 保留既有调用签名，其复核已退化为同一个 cwd 检查（文件对象身份
    /// 与完整发现快照不再参与准入）。因此「远端 binding 指向的本机对象」与「本机会话
    /// 的绑定」不会出现两套结论。
    pub(super) async fn validate_binding_value_impl(
        &self,
        binding: &SessionBinding,
        full: bool,
    ) -> Result<ResolvedWorkspace> {
        let mut connection = self.pool.acquire().await?;
        let workspace = match Self::validate_binding_relation_on(&mut connection, binding).await {
            Ok(workspace) => workspace,
            Err(error) => return Err(normalize_binding_lookup(error)),
        };
        if full {
            Self::revalidate_registered_observation_on(&mut connection, &workspace).await?;
        }
        Ok(workspace)
    }

    pub(super) async fn list_scoped_threads_impl(
        &self,
        query: &ScopedThreadQuery,
    ) -> Result<ScopedThreadPage> {
        self.list_scoped_threads_by_archive_impl(query, false).await
    }

    pub(super) async fn list_scoped_threads_by_archive_impl(
        &self,
        query: &ScopedThreadQuery,
        archived: bool,
    ) -> Result<ScopedThreadPage> {
        let limit = query.limit.clamp(1, 200) as usize;
        let mut sql: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT t.id, t.title, t.message_count, t.updated_at,
            b.schema_version, b.project_id, b.workspace_id, b.relative_cwd, w.path, t.cwd, b.thread_id
            FROM threads t LEFT JOIN session_bindings b ON b.thread_id = t.id LEFT JOIN workspaces w ON w.id = t.workspace_id
            WHERE t.parent_thread_id IS NULL AND t.hidden = 0 AND t.archived = ");
        sql.push_bind(archived);
        sql.push(" AND t.message_count > 0");
        match &query.scope {
            ThreadScope::Environment(machine_id) => {
                sql.push(" AND EXISTS (SELECT 1 FROM workspaces owner WHERE owner.id = t.workspace_id AND owner.machine_id = ")
                    .push_bind(machine_id)
                    .push(")");
            }
            ThreadScope::Project(id) => {
                sql.push(" AND (b.project_id = ").push_bind(id.to_string());
                push_legacy_scope(&mut sql, "project_id", id.to_string());
            }
            ThreadScope::Workspace(id) => {
                sql.push(" AND t.workspace_id = ").push_bind(id.to_string());
            }
            ThreadScope::ExactDirectory {
                workspace_id,
                relative_cwd,
            } => {
                validate_relative(relative_cwd)?;
                sql.push(" AND ((t.workspace_id = ")
                    .push_bind(workspace_id.to_string())
                    .push(" AND b.relative_cwd = ")
                    .push_bind(discovery::path_text(relative_cwd)?)
                    .push(") OR (b.thread_id IS NULL AND EXISTS (SELECT 1 FROM workspaces legacy WHERE legacy.id = ")
                    .push_bind(workspace_id.to_string())
                    .push(" AND ").push(legacy_path_sql("t.cwd"))
                    .push(" = ").push(legacy_path_sql("legacy.path"));
                if !relative_cwd.as_os_str().is_empty() {
                    sql.push(" || '/' || ").push_bind(if cfg!(windows) {
                        discovery::path_text(relative_cwd)?.replace('\\', "/")
                    } else {
                        discovery::path_text(relative_cwd)?.to_owned()
                    });
                }
                sql.push(")))");
            }
            ThreadScope::All => {}
        }
        if let Some(cursor) = &query.cursor {
            sql.push(" AND (t.updated_at, t.id) < (")
                .push_bind(cursor.updated_at.to_rfc3339())
                .push(", ")
                .push_bind(&cursor.thread_id)
                .push(")");
        }
        sql.push(" ORDER BY t.updated_at DESC, t.id DESC LIMIT ")
            .push_bind((limit + 1) as i64);
        let rows = sql.build().fetch_all(&self.pool).await?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let (binding, root, effective_cwd) = if row.try_get::<Option<String>, _>(10)?.is_some()
            {
                let binding = decode_binding((
                    row.try_get(4)?,
                    row.try_get(5)?,
                    row.try_get(6)?,
                    row.try_get(7)?,
                ))?;
                let root = PathBuf::from(row.try_get::<String, _>(8)?);
                let cwd = binding_cwd(&root, &binding.cwd_relative_to_workspace);
                (Some(binding), Some(root), cwd)
            } else {
                (None, None, PathBuf::from(row.try_get::<String, _>(9)?))
            };
            let count: i64 = row.try_get(2)?;
            entries.push(ScopedThreadEntry {
                thread: ThreadListEntry {
                    id: row.try_get(0)?,
                    title: row.try_get(1)?,
                    cwd: discovery::path_text(&effective_cwd)?.to_owned(),
                    message_count: usize::try_from(count).context("negative message count")?,
                    updated_at: row.try_get::<String, _>(3)?.parse()?,
                },
                binding,
                effective_cwd,
                workspace_root: root,
            });
        }
        let has_more = entries.len() > limit;
        entries.truncate(limit);
        let next_cursor = if has_more {
            entries.last().map(|entry| ThreadListCursor {
                updated_at: entry.thread.updated_at,
                thread_id: entry.thread.id.clone(),
            })
        } else {
            None
        };
        Ok(ScopedThreadPage {
            entries,
            next_cursor,
        })
    }
}

/// Legacy paths are display associations only. EXISTS avoids duplicates for overlapping roots;
/// substring equality treats SQL wildcard characters as ordinary path characters.
///
/// 无绑定的历史会话按归属行的路径与项目证据归组：v19 之前这组关系由执行登记表提供，
/// 现在归属行就是那份事实（`project_id` = 证据列，`path` = 归属路径）。
fn push_legacy_scope(sql: &mut QueryBuilder<Sqlite>, column: &str, id: String) {
    let cwd = legacy_path_sql("t.cwd");
    let root = legacy_path_sql("legacy.path");
    sql.push(" OR (b.thread_id IS NULL AND EXISTS (SELECT 1 FROM workspaces legacy WHERE legacy.")
        .push(column)
        .push(" = ")
        .push_bind(id)
        .push(format!(
            " AND ({cwd} = {root} OR substr({cwd}, 1, length({root}) + 1) = {root} || '/'))))"
        ));
}

/// Only a display comparison: never use this normalization as execution identity.
fn legacy_path_sql(column: &str) -> String {
    #[cfg(windows)]
    let column = windows_legacy_path_sql(column);
    #[cfg(target_os = "macos")]
    let column = format!(
        "CASE WHEN {column} IN ('/private/var', '/private/tmp', '/private/etc') OR substr({column}, 1, 13) IN ('/private/var/', '/private/tmp/', '/private/etc/') THEN substr({column}, 9) ELSE {column} END"
    );
    format!("rtrim({column}, '/')")
}

#[cfg(any(windows, test))]
fn windows_legacy_path_sql(column: &str) -> String {
    let path = format!("replace({column}, char(92), '/')");
    format!("(CASE WHEN substr({path}, 1, 8) = '//?/UNC/' THEN '//' || substr({path}, 9) WHEN substr({path}, 1, 4) = '//?/' THEN substr({path}, 5) ELSE {path} END) COLLATE NOCASE")
}

#[tokio::test]
async fn legacy_windows_path_comparison_accepts_verbatim_drive_and_unc() {
    use sqlx::Connection;
    let mut connection = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    let left = windows_legacy_path_sql("?1");
    let right = windows_legacy_path_sql("?2");
    let sql = format!("SELECT rtrim({left}, '/') = rtrim({right}, '/')");
    for (saved, registered, matches) in [
        (r"C:\repo", r"\\?\C:\repo", true),
        ("c:/repo/", r"\\?\C:\repo", true),
        (r"\\server\share\repo", r"\\?\UNC\server\share\repo", true),
        (r"C:\repo-other", r"\\?\C:\repo", false),
    ] {
        let (equal,): (bool,) = QueryBuilder::<Sqlite>::new(&sql)
            .build_query_as()
            .bind(saved)
            .bind(registered)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(equal, matches, "{saved} vs {registered}");
    }
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;

/// `workspaces` 里查不到这条绑定引用的工作区：那是「本机不认识这个绑定」，
/// 不是内部错误。未登记的 project/workspace 因此得到 workspace 语义的失败。
fn normalize_binding_lookup(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<sqlx::Error>() {
        Some(sqlx::Error::RowNotFound) => WorkspaceError::InvalidBinding.into(),
        _ => error,
    }
}
