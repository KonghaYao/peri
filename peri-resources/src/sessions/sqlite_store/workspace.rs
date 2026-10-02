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
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        // 登记键是 canonical root 路径加上该目录的文件对象证据，两者同时命中才复用
        // 原登记。目录被替换（同路径的新对象）或换位（同一对象的新路径）都不命中原
        // 登记，但它们是可访问的目录：为其建立新登记，执行 cwd、项目归属和已有绑定
        // 都不移动——引用旧登记的会话继续按各自证据复核，不会静默改绑。
        let registered: Option<(String, String, String)> = sqlx::query_as(
            "SELECT id, project_id, discovery FROM legacy_execution_registrations WHERE root = ? AND root_identity = ?",
        )
        .bind(root)
        .bind(&root_identity)
        .fetch_optional(&mut *tx)
        .await?;
        let (workspace_id, project_id) = match registered {
            Some((id, project, recorded)) => {
                if recorded != snapshot && !self.read_only {
                    // A directory-only observation without Git answering cannot prove
                    // the recorded repository is gone. Keep failing closed instead of
                    // rewriting a repository into a directory.
                    if !git_answered {
                        return Err(WorkspaceError::NeedsRelink.into());
                    }
                    sqlx::query(
                        "UPDATE legacy_execution_registrations SET discovery = ? WHERE id = ?",
                    )
                    .bind(&snapshot)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
                }
                (id.parse::<WorkspaceId>()?, project.parse::<ProjectId>()?)
            }
            None => {
                // 只读节点不能登记新工作区：读请求按「本节点没有这条登记」失败，而不是
                // 交给 SQLite 在写入时才报只读。
                self.require_writable()?;
                // 只有定位与证据同时一致才复用项目：Git linked worktree 换位后
                // common directory 未变而路径已变，它仍属于原项目，而不相关的
                // 同名副本各自成项目。
                let existing: Option<(String,)> = sqlx::query_as(
                    "SELECT id FROM projects WHERE locator = ? AND object_identity = ?",
                )
                .bind(locator)
                .bind(&identity)
                .fetch_optional(&mut *tx)
                .await?;
                let project_id = match existing {
                    Some((id,)) => id.parse::<ProjectId>()?,
                    None => {
                        let id = ProjectId::new();
                        sqlx::query(
                            "INSERT INTO projects (id, locator, object_identity) VALUES (?, ?, ?)",
                        )
                        .bind(id.to_string())
                        .bind(locator)
                        .bind(&identity)
                        .execute(&mut *tx)
                        .await?;
                        id
                    }
                };
                let id = WorkspaceId::new();
                sqlx::query("INSERT INTO legacy_execution_registrations (id, project_id, root, root_identity, discovery) VALUES (?, ?, ?, ?, ?)")
                    .bind(id.to_string()).bind(project_id.to_string()).bind(root).bind(&root_identity).bind(&snapshot).execute(&mut *tx).await?;
                (id, project_id)
            }
        };
        // The write transaction is the registry's common admission point: a changed
        // filesystem observation cannot commit a stale winner while another host
        // registers. External probing stays outside the lock — the revalidation here
        // re-checks the critical file objects only, so a slow or missing Git never
        // blocks other writers of the same database.
        let machine = crate::sessions::machine::current()?;
        let owner_workspace_id = if self.read_only {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT id FROM workspaces WHERE machine_id = ?1 AND path = ?2")
                    .bind(machine)
                    .bind(root)
                    .fetch_optional(&mut *tx)
                    .await?;
            row.ok_or(WorkspaceError::ReadOnlyStore)?.0.parse()?
        } else {
            super::workspace_identity::ensure_current_machine(&mut tx).await?;
            super::workspace_identity::resolve_identity(
                &mut tx,
                machine,
                root,
                if discovered.common_dir.is_some() || discovered.private_dir.is_some() {
                    "discovered"
                } else {
                    "unverified"
                },
            )
            .await?
        };
        discovered.reassert_key_objects(&cwd).await?;
        tx.commit().await?;
        let relative_cwd = cwd
            .strip_prefix(&discovered.root)
            .map_err(|_| WorkspaceError::NeedsRelink)?
            .to_path_buf();
        Ok(ResolvedWorkspace {
            project_id,
            workspace_id: owner_workspace_id,
            execution_registration_id: workspace_id,
            cwd,
            root: discovered.root,
            relative_cwd,
            discovery_snapshot: Some(snapshot),
        })
    }

    /// 事务内的复核：SQL 关系加关键文件对象，不启动外部进程。
    ///
    /// Transaction callers reuse their admitted connection, including every SQL read.
    async fn validate_resolved_on(
        connection: &mut SqliteConnection,
        workspace: &ResolvedWorkspace,
    ) -> Result<()> {
        validate_relative(&workspace.relative_cwd)?;
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT project_id, root, discovery FROM legacy_execution_registrations WHERE id = ?",
        )
        .bind(workspace.execution_registration_id.to_string())
        .fetch_optional(&mut *connection)
        .await?;
        let (project, root, snapshot) = row.ok_or(WorkspaceError::InvalidBinding)?;
        if project != workspace.project_id.to_string()
            || Path::new(&root) != workspace.root
            || binding_cwd(&workspace.root, &workspace.relative_cwd) != workspace.cwd
        {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        let owner: Option<(String, String)> =
            sqlx::query_as("SELECT machine_id, path FROM workspaces WHERE id = ?1")
                .bind(workspace.workspace_id.to_string())
                .fetch_optional(&mut *connection)
                .await?;
        let current_machine = crate::sessions::machine::current()?;
        if !owner.is_some_and(|(machine, path)| {
            machine == current_machine && Path::new(&path) == workspace.root
        }) {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        let discovered: Discovery =
            serde_json::from_str(&snapshot).map_err(|_| WorkspaceError::InvalidBinding)?;
        discovered.reassert_key_objects(&workspace.cwd).await
    }

    /// 事务外的完整快照复核：重新执行 Git 发现并与已登记观测比对（设计 §3.2）。
    ///
    /// 只能在持有写事务之外调用；事务内的复核见 `validate_resolved_on`。
    async fn revalidate_registered_observation_on(
        connection: &mut SqliteConnection,
        workspace: &ResolvedWorkspace,
    ) -> Result<()> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT discovery FROM legacy_execution_registrations WHERE id = ? AND project_id = ?",
        )
        .bind(workspace.execution_registration_id.to_string())
        .bind(workspace.project_id.to_string())
        .fetch_optional(&mut *connection)
        .await?;
        let (snapshot,) = row.ok_or(WorkspaceError::InvalidBinding)?;
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
        let write_guard = if let Some(parent) = &meta.parent_thread_id {
            // 桥与本机数据面共用同一个库：父线程的事实（含它是谁的子会话）按本机读法取。
            let facts = self.local_session_facts(parent).await?;
            let guard = self.require_execution_lease(parent, &facts).await?;
            // 子线程继承父线程的同一工作区：比对的是已记录的绑定身份，不需要重新发现。
            let parent_workspace = self.reassert_session_binding_impl(parent).await;
            match parent_workspace {
                Ok(parent_workspace) if &parent_workspace == workspace => guard,
                other => {
                    if let Some(guard) = guard {
                        guard.finish();
                    }
                    return match other {
                        Err(error) => Err(error),
                        Ok(_) => Err(WorkspaceError::ExecutionBindingMismatch.into()),
                    };
                }
            }
        } else {
            None
        };
        let result = async {
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
            VALUES (?1, ?2, ?3, ?4, ?5,
            (SELECT discovery FROM legacy_execution_registrations WHERE id = ?4), 'creation_snapshot')")
            .bind(&meta.id).bind(i64::from(SESSION_BINDING_VERSION)).bind(workspace.project_id.to_string())
            .bind(workspace.execution_registration_id.to_string()).bind(discovery::path_text(&workspace.relative_cwd)?)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(meta.id)
        }.await;
        if let Some(guard) = write_guard {
            guard.finish();
        }
        result
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
            return Err(WorkspaceError::ExecutionLeaseRequired.into());
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
                VALUES (?1, ?2, ?3, ?4, ?5,
                    (SELECT discovery FROM legacy_execution_registrations WHERE id = ?4), 'legacy_last_observation')")
                .bind(id).bind(i64::from(SESSION_BINDING_VERSION))
                .bind(workspace.project_id.to_string()).bind(workspace.execution_registration_id.to_string())
                .bind(discovery::path_text(&workspace.relative_cwd)?)
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

    /// 绑定值的本机复核：登记关系加关键文件对象。
    ///
    /// 与 [`Self::validate_session_binding_on`] 的差别只在于事实来源：那个从已保存的
    /// binding 行读，这个复核调用方手上的 binding 值（新建/fork/child 在写入前用它，
    /// 避免未登记的 project/workspace 直接落到外键失败上）。
    pub(super) async fn validate_binding_relation_on(
        connection: &mut SqliteConnection,
        binding: &SessionBinding,
    ) -> Result<ResolvedWorkspace> {
        let row: (String, String) = sqlx::query_as(
            "SELECT root, discovery FROM legacy_execution_registrations WHERE id = ? AND project_id = ?",
        )
        .bind(binding.workspace_id.to_string())
        .bind(binding.project_id.to_string())
        .fetch_one(&mut *connection)
        .await?;
        let root = PathBuf::from(&row.0);
        let owner: (String,) =
            sqlx::query_as("SELECT id FROM workspaces WHERE machine_id = ?1 AND path = ?2")
                .bind(crate::sessions::machine::current()?)
                .bind(&row.0)
                .fetch_one(&mut *connection)
                .await?;
        let workspace = ResolvedWorkspace {
            project_id: binding.project_id,
            workspace_id: owner.0.parse()?,
            execution_registration_id: binding.workspace_id,
            cwd: binding_cwd(&root, &binding.cwd_relative_to_workspace),
            root,
            relative_cwd: binding.cwd_relative_to_workspace.clone(),
            discovery_snapshot: Some(row.1),
        };
        Self::validate_resolved_on(connection, &workspace).await?;
        Ok(workspace)
    }

    /// 绑定**值**的本机复核（远程组合：绑定来自远端会话行，不在本机 `session_bindings`）。
    ///
    /// 判定与本机绑定完全同一套：登记关系（project/workspace 必须在本机登记过）加关键文件
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
            b.schema_version, b.project_id, b.workspace_id, b.relative_cwd, w.root, t.cwd, b.thread_id
            FROM threads t LEFT JOIN session_bindings b ON b.thread_id = t.id LEFT JOIN legacy_execution_registrations w ON w.id = b.workspace_id
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
fn push_legacy_scope(sql: &mut QueryBuilder<Sqlite>, column: &str, id: String) {
    let cwd = legacy_path_sql("t.cwd");
    let root = legacy_path_sql("legacy.root");
    sql.push(" OR (b.thread_id IS NULL AND EXISTS (SELECT 1 FROM legacy_execution_registrations legacy WHERE legacy.")
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
