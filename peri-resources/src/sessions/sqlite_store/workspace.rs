//! Registry and session binding transactions; SQL-scoped lightweight history pages.

use super::{
    discovery::{self, Discovery},
    SqliteThreadStore,
};
use anyhow::{Context, Result};
use peri_acp_types::{
    thread::{ThreadId, ThreadListEntry, ThreadMeta},
    workspace::*,
};
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};
use std::path::{Component, Path, PathBuf};

type BindingRow = (i64, String, String, String);

fn decode_binding(row: BindingRow) -> Result<SessionBinding> {
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

fn validate_relative(path: &Path) -> Result<()> {
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(WorkspaceError::InvalidBinding.into());
    }
    discovery::path_text(path)?;
    Ok(())
}

impl SqliteThreadStore {
    pub(super) async fn resolve_workspace_impl(&self, cwd: &Path) -> Result<ResolvedWorkspace> {
        let (cwd, discovered) = discovery::discover(cwd).await?;
        let root = discovery::path_text(&discovered.root)?;
        let locator = discovery::path_text(discovered.project_locator())?;
        let identity = serde_json::to_string(discovered.project_identity())?;
        let snapshot = serde_json::to_string(&discovered)?;
        let root_identity = serde_json::to_string(&discovered.root_identity)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let existing: Option<(String, String, String)> = sqlx::query_as("SELECT id, object_identity, locator FROM projects WHERE locator = ? OR object_identity = ?")
            .bind(locator).bind(&identity).fetch_optional(&mut *tx).await?;
        let project_id = match existing {
            Some((id, evidence, registered_locator))
                if evidence == identity && registered_locator == locator =>
            {
                id.parse::<ProjectId>()?
            }
            Some(_) => return Err(WorkspaceError::NeedsRelink.into()),
            None => {
                let id = ProjectId::new();
                sqlx::query("INSERT INTO projects (id, locator, object_identity) VALUES (?, ?, ?)")
                    .bind(id.to_string())
                    .bind(locator)
                    .bind(&identity)
                    .execute(&mut *tx)
                    .await?;
                id
            }
        };
        let existing: Option<(String, String, String)> = sqlx::query_as(
            "SELECT id, project_id, discovery FROM workspaces WHERE root = ? OR root_identity = ?",
        )
        .bind(root)
        .bind(&root_identity)
        .fetch_optional(&mut *tx)
        .await?;
        let workspace_id = match existing {
            Some((id, project, evidence))
                if project == project_id.to_string() && evidence == snapshot =>
            {
                id.parse::<WorkspaceId>()?
            }
            Some(_) => return Err(WorkspaceError::NeedsRelink.into()),
            None => {
                let id = WorkspaceId::new();
                sqlx::query("INSERT INTO workspaces (id, project_id, root, root_identity, discovery) VALUES (?, ?, ?, ?, ?)")
                    .bind(id.to_string()).bind(project_id.to_string()).bind(root).bind(&root_identity).bind(&snapshot).execute(&mut *tx).await?;
                id
            }
        };
        // The write transaction is the registry's common admission point. A changed
        // filesystem observation cannot commit a stale winner while another host registers.
        discovered.revalidate(&cwd).await?;
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
        })
    }

    async fn validate_resolved(&self, workspace: &ResolvedWorkspace) -> Result<()> {
        let mut connection = self.pool.acquire().await?;
        Self::validate_resolved_on(&mut connection, workspace).await
    }

    /// Transaction callers reuse their admitted connection, including every SQL read.
    async fn validate_resolved_on(
        connection: &mut SqliteConnection,
        workspace: &ResolvedWorkspace,
    ) -> Result<()> {
        validate_relative(&workspace.relative_cwd)?;
        let row: Option<(String, String, String)> =
            sqlx::query_as("SELECT project_id, root, discovery FROM workspaces WHERE id = ?")
                .bind(workspace.workspace_id.to_string())
                .fetch_optional(&mut *connection)
                .await?;
        let (project, root, snapshot) = row.ok_or(WorkspaceError::InvalidBinding)?;
        if project != workspace.project_id.to_string()
            || Path::new(&root) != workspace.root
            || workspace.root.join(&workspace.relative_cwd) != workspace.cwd
        {
            return Err(WorkspaceError::ExecutionBindingMismatch.into());
        }
        let discovered: Discovery =
            serde_json::from_str(&snapshot).map_err(|_| WorkspaceError::InvalidBinding)?;
        let canonical = tokio::fs::canonicalize(&workspace.cwd)
            .await
            .map_err(|_| WorkspaceError::Unavailable)?;
        if canonical != workspace.cwd {
            return Err(WorkspaceError::NeedsRelink.into());
        }
        discovered.revalidate(&workspace.cwd).await
    }

    pub(super) async fn create_bound_thread_impl(
        &self,
        mut meta: ThreadMeta,
        workspace: &ResolvedWorkspace,
    ) -> Result<ThreadId> {
        self.validate_resolved(workspace).await?;
        let write_guard = if let Some(parent) = &meta.parent_thread_id {
            let guard = self.require_execution_lease(parent).await?;
            let parent_workspace = self.validate_session_binding_impl(parent).await;
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
            parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, cached_context, agent_status)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&meta.id).bind(&meta.title).bind(&meta.cwd).bind(meta.created_at.to_rfc3339()).bind(meta.updated_at.to_rfc3339())
            .bind(meta.message_count as i64).bind(&meta.parent_thread_id).bind(&meta.snapshot_at_message_id).bind(meta.hidden)
            .bind(meta.cancel_policy.as_str()).bind(&meta.config).bind(&meta.cached_context).bind(meta.agent_status.as_str())
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO session_bindings (thread_id, schema_version, project_id, workspace_id, relative_cwd)
            VALUES (?, ?, ?, ?, ?)")
            .bind(&meta.id).bind(i64::from(SESSION_BINDING_VERSION)).bind(workspace.project_id.to_string())
            .bind(workspace.workspace_id.to_string()).bind(discovery::path_text(&workspace.relative_cwd)?)
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

    pub(super) async fn validate_session_binding_impl(
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
        let row: (String,) =
            sqlx::query_as("SELECT root FROM workspaces WHERE id = ? AND project_id = ?")
                .bind(binding.workspace_id.to_string())
                .bind(binding.project_id.to_string())
                .fetch_one(&mut *connection)
                .await?;
        let root = PathBuf::from(row.0);
        let workspace = ResolvedWorkspace {
            project_id: binding.project_id,
            workspace_id: binding.workspace_id,
            cwd: root.join(&binding.cwd_relative_to_workspace),
            root,
            relative_cwd: binding.cwd_relative_to_workspace,
        };
        Self::validate_resolved_on(connection, &workspace).await?;
        Ok(workspace)
    }

    pub(super) async fn list_scoped_threads_impl(
        &self,
        query: &ScopedThreadQuery,
    ) -> Result<ScopedThreadPage> {
        let limit = query.limit.clamp(1, 200) as usize;
        let mut sql: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT t.id, t.title, t.message_count, t.updated_at,
            b.schema_version, b.project_id, b.workspace_id, b.relative_cwd, w.root
            FROM threads t JOIN session_bindings b ON b.thread_id = t.id JOIN workspaces w ON w.id = b.workspace_id
            WHERE t.hidden = 0 AND t.message_count > 0");
        match &query.scope {
            ThreadScope::Project(id) => {
                sql.push(" AND b.project_id = ").push_bind(id.to_string());
            }
            ThreadScope::Workspace(id) => {
                sql.push(" AND b.workspace_id = ").push_bind(id.to_string());
            }
            ThreadScope::ExactDirectory {
                workspace_id,
                relative_cwd,
            } => {
                validate_relative(relative_cwd)?;
                sql.push(" AND b.workspace_id = ")
                    .push_bind(workspace_id.to_string())
                    .push(" AND b.relative_cwd = ")
                    .push_bind(discovery::path_text(relative_cwd)?);
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
            let binding = decode_binding((
                row.try_get(4)?,
                row.try_get(5)?,
                row.try_get(6)?,
                row.try_get(7)?,
            ))?;
            let root = PathBuf::from(row.try_get::<String, _>(8)?);
            let effective_cwd = root.join(&binding.cwd_relative_to_workspace);
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

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
