//! Registry and session binding transactions; SQL-scoped lightweight history pages.

use super::{
    database::SqliteSessionDatabase,
    discovery::{self, Discovery, Observation},
};
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
/// 是当前占用该路径的文件对象证据，不参与身份。只读节点按 NULL 判「本机不认识这个目录」。
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

/// 绑定指向的执行目录：相对路径为空时就是工作区根本身。
///
/// 不能直接写 `root.join(relative)`：`join("")` 会追加分隔符（`/a/b` → `/a/b/`），
/// 同一个目录因此出现两种文本形式——登记解析返回不带分隔符的形式，绑定复核返回
/// 带分隔符的形式。调用方按字符串比较目录（如 TUI 的线程列表缓存工作区解析结果）
/// 会把同一个目录当成换了目录，为它重跑一次本应只做一次的完整发现。
fn binding_cwd(root: &Path, relative: &Path) -> PathBuf {
    if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    }
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
        // 归属键是 `(machine_id, path)`：同机同路径只有一个 Workspace。执行证据（项目、
        // 文件对象身份、发现快照）只是该行记录**当前占用这个路径的对象**，不参与身份：
        // 目录被替换（同路径的新对象）或换位（同一对象的新路径）都不移动归属，已有绑定
        // 继续按各自创建时的证据复核，不会静默改绑。
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
            Some((id, project, recorded, recorded_identity)) => {
                // 目录模式的变化必须以 Git 的回答为准：只读目录观测（Git 没回答）不能
                // 证明已记录的仓库消失了，保持 fail-closed，而不是把仓库改写成普通目录。
                if recorded.as_deref() != Some(snapshot.as_str()) && !git_answered {
                    return Err(WorkspaceError::NeedsRelink.into());
                }
                // 项目归属只在**该路径换了文件对象**时随证据移动：对象身份没变，说明这是
                // 同一个目录的派生观测变化（`git init`、移除 `.git`），原项目继续复用
                // （设计 §3.3），已有绑定的项目关系因此保持成立。
                let project_id = match project {
                    Some(project)
                        if recorded_identity.as_deref() == Some(root_identity.as_str()) =>
                    {
                        project.parse::<ProjectId>()?
                    }
                    _ => Self::observation_project(&mut tx, locator, &identity).await?,
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

    /// 本次观测的项目记录：定位与对象证据同时一致才复用（Git linked worktree 换位后
    /// common directory 未变而路径已变，它仍属于原项目；不相关的同名副本各自成项目）。
    async fn observation_project(
        connection: &mut SqliteConnection,
        locator: &str,
        identity: &str,
    ) -> Result<ProjectId> {
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM projects WHERE locator = ? AND object_identity = ?")
                .bind(locator)
                .bind(identity)
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

    /// 事务内的复核：SQL 关系加关键文件对象，不启动外部进程。
    ///
    /// 关系读的是归属行本身（`workspaces.id` = 归属身份）：项目证据、路径与机器三样
    /// 必须与本次准入手上的 workspace 一致。关键文件对象以该行记录的执行证据为基线；
    /// 行上还没有证据时退回本次观测的快照，两者都没有就按 fail-closed 要求重新解析。
    ///
    /// 项目证据允许为空（远端写入的归属行可能只有路径）：空值在这里是一致性比较的
    /// 不匹配，不是解码失败——按「行不再记录这次准入的对象」处理，不落到内部错误上。
    ///
    /// Transaction callers reuse their admitted connection, including every SQL read.
    async fn validate_resolved_on(
        connection: &mut SqliteConnection,
        workspace: &ResolvedWorkspace,
    ) -> Result<()> {
        validate_relative(&workspace.relative_cwd)?;
        let row: Option<(Option<String>, String, String, Option<String>)> = sqlx::query_as(
            "SELECT project_id, machine_id, path, discovery FROM workspaces WHERE id = ?1",
        )
        .bind(workspace.workspace_id.to_string())
        .fetch_optional(&mut *connection)
        .await?;
        let (project, machine, path, recorded) = row.ok_or(WorkspaceError::InvalidBinding)?;
        if project.as_deref() != Some(workspace.project_id.to_string().as_str())
            || Path::new(&path) != workspace.root
            || binding_cwd(&workspace.root, &workspace.relative_cwd) != workspace.cwd
        {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        if machine != crate::sessions::machine::current()? {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        let snapshot = recorded
            .or_else(|| workspace.discovery_snapshot.clone())
            .ok_or(WorkspaceError::NeedsRelink)?;
        let discovered: Discovery =
            serde_json::from_str(&snapshot).map_err(|_| WorkspaceError::InvalidBinding)?;
        discovered.reassert_key_objects(&workspace.cwd).await
    }

    /// 事务外的完整快照复核：重新执行 Git 发现并与归属行记录的执行证据比对（设计 §3.2）。
    ///
    /// 只能在持有写事务之外调用；事务内的复核见 `validate_resolved_on`。行上还没有执行
    /// 证据（或证据读不出来）时按 `NeedsRelink` 失败关闭：本机证明不了这个目录是绑定
    /// 时那个对象，就不能拿别的观测替代。
    async fn revalidate_registered_observation_on(
        connection: &mut SqliteConnection,
        workspace: &ResolvedWorkspace,
    ) -> Result<()> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT discovery FROM workspaces WHERE id = ?1")
                .bind(workspace.workspace_id.to_string())
                .fetch_optional(&mut *connection)
                .await?;
        let (snapshot,) = row.ok_or(WorkspaceError::InvalidBinding)?;
        let snapshot = snapshot.ok_or(WorkspaceError::NeedsRelink)?;
        let discovered: Discovery =
            serde_json::from_str(&snapshot).map_err(|_| WorkspaceError::InvalidBinding)?;
        discovered.revalidate(&workspace.cwd).await
    }

    pub(super) async fn create_bound_thread_impl(
        &self,
        mut meta: ThreadMeta,
        workspace: &ResolvedWorkspace,
    ) -> Result<ThreadId> {
        self.require_writable()?;
        // 提交前的复核在写事务内进行（`validate_resolved_on`：关系加关键文件对象）。
        // 同一次准入已在解析阶段观测过完整发现，这里再跑一轮 Git 只是把同一次观测
        // 重复一遍，代价是每个创建方都要等 Git（含慢 Git 的固定等待）。
        if let Some(parent) = &meta.parent_thread_id {
            let parent_workspace = self.reassert_session_binding_impl(parent).await?;
            if &parent_workspace != workspace {
                return Err(WorkspaceError::ExecutionBindingMismatch.into());
            }
        }
        meta.cwd = discovery::path_text(&workspace.cwd)?.to_owned();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::validate_resolved_on(&mut tx, workspace).await?;
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
        let (cwd, parent, frozen, owner): (String, Option<String>, Option<String>, String) = sqlx::query_as(
            "SELECT cwd, parent_thread_id, frozen_context, workspace_id FROM threads WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if cwd != saved_cwd || parent.is_some() || owner != workspace.workspace_id.to_string() {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
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
            if Self::validate_session_binding_on(&mut tx, id).await? != *workspace {
                return Err(WorkspaceError::ExecutionBindingMismatch.into());
            }
        } else {
            super::session_data::validate_unbound_legacy_frozen(frozen.as_deref())?;
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

    /// 已有绑定的权威复核：关系、关键文件对象加一次完整发现快照比对。
    ///
    /// 这是**一次准入的复核动作**：调用方（`session/load`、prompt 轮、`workflow/resume`
    /// 等）以此为本次准入判定的全部依据，一次准入只应调用一次。准入内的后续检查用
    /// `reassert_session_binding_impl`。
    pub(super) async fn validate_session_binding_impl(
        &self,
        id: &ThreadId,
    ) -> Result<ResolvedWorkspace> {
        let mut connection = self.pool.acquire().await?;
        let workspace = Self::validate_session_binding_on(&mut connection, id).await?;
        // 事务外才叠加完整快照复核，写事务内不做 Git 探测（设计 §3.2）。
        let snapshot: (Option<String>,) =
            sqlx::query_as("SELECT discovery_snapshot FROM session_bindings WHERE thread_id = ?1")
                .bind(id)
                .fetch_one(&mut *connection)
                .await?;
        let snapshot = snapshot.0.ok_or(WorkspaceError::NeedsRelink)?;
        let discovered: Discovery =
            serde_json::from_str(&snapshot).map_err(|_| WorkspaceError::InvalidBinding)?;
        discovered.revalidate(&workspace.cwd).await?;
        Ok(workspace)
    }

    /// 同一次准入内的复核：SQL 关系加关键文件对象，不启动外部进程。
    ///
    /// 与 `validate_session_binding_impl` 的差别只有一处：不重新执行 Git 发现。准入已经
    /// 观测过完整快照并复核过，重复发现只会把 Git 的等待（慢盘、慢 Git、Git 缺失时的
    /// 探测）叠加到同一次准入的每一步上，不会带来新的证据。目录被替换、被换位或 Git
    /// 位置消失仍会在这里失败——这些都由关键文件对象身份覆盖。
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
        let snapshot: (Option<String>,) =
            sqlx::query_as("SELECT discovery_snapshot FROM session_bindings WHERE thread_id = ?1")
                .bind(id)
                .fetch_one(&mut *connection)
                .await?;
        let snapshot = snapshot.0.ok_or(WorkspaceError::NeedsRelink)?;
        let discovered: Discovery =
            serde_json::from_str(&snapshot).map_err(|_| WorkspaceError::InvalidBinding)?;
        if discovered.root != workspace.root {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        discovered.reassert_key_objects(&workspace.cwd).await?;
        Ok(workspace)
    }

    /// 绑定值的本机复核：归属关系加关键文件对象。
    ///
    /// 与 [`Self::validate_session_binding_on`] 的差别只在于事实来源：那个从已保存的
    /// binding 行读，这个复核调用方手上的 binding 值（新建/fork/child 在写入前用它，
    /// 避免未登记的 project/workspace 直接落到外键失败上）。
    ///
    /// 归属身份就是归属行的 `id`（`(machine_id, path)`）：查不到这行，或这行属于别的
    /// 机器，都是「本机不认识这个绑定」——与 v19 之前查不到执行登记同一类失败，保持
    /// `InvalidBinding`，不升级成内部错误。
    ///
    /// 行还在、机器也对，但行上记录的项目证据（当前占用该路径的文件对象）与绑定不同：
    /// 绑定创建时那个对象已经不在这个路径上（目录被替换、删除后重建）。归属身份仍然
    /// 成立，但这次绑定不能在这里执行，也不自动改绑——按 `NeedsRelink` fail-closed，
    /// 与 v19 之前「登记证据复核不过」得到同一个结论。没有项目证据的行同理：本机无法
    /// 证明这个目录是绑定时那个对象。
    pub(super) async fn validate_binding_relation_on(
        connection: &mut SqliteConnection,
        binding: &SessionBinding,
    ) -> Result<ResolvedWorkspace> {
        let row: Option<(String, Option<String>, String, Option<String>)> = sqlx::query_as(
            "SELECT path, project_id, machine_id, discovery FROM workspaces WHERE id = ?1",
        )
        .bind(binding.workspace_id.to_string())
        .fetch_optional(&mut *connection)
        .await?;
        let (path, project, machine, discovery) = row.ok_or(WorkspaceError::InvalidBinding)?;
        if machine != crate::sessions::machine::current()? {
            return Err(WorkspaceError::InvalidBinding.into());
        }
        if project.as_deref() != Some(binding.project_id.to_string().as_str()) {
            return Err(WorkspaceError::NeedsRelink.into());
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
    /// 判定与本机绑定完全同一套：归属关系（project/workspace 必须在本机登记过）加关键文件
    /// 对象身份，`full` 时再叠一次完整发现快照比对。因此「远端 binding 指向的本机对象」与
    /// 「本机会话的绑定」不会出现两套结论。
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
