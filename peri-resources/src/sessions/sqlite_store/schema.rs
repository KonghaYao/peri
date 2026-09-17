//! 单库 schema 升级：保留历史与执行状态，事务内调整结构。

use super::SqliteThreadStore;
use anyhow::Result;
use peri_acp_types::workspace::WorkspaceError;
use sqlx::{AssertSqlSafe, SqliteConnection};
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SchemaState {
    Empty,
    Legacy,
    Version2,
    Version3,
    Current,
}

/// 旧版未设置 user_version；校验本模块所需基础表，保留同库的其他业务表。
pub(super) async fn inspect(connection: &mut SqliteConnection) -> Result<SchemaState> {
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await?;
    match version {
        4 => return Ok(SchemaState::Current),
        3 => return Ok(SchemaState::Version3),
        2 => return Ok(SchemaState::Version2),
        0 => {}
        _ => return Err(WorkspaceError::UnsupportedDatabaseSchema.into()),
    }
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut *connection)
    .await?;
    if tables.is_empty() {
        return Ok(SchemaState::Empty);
    }
    // 只要求必需的真实表存在，不限制整库的表集合；VIEW 不能替代可升级的表。
    if !["threads", "messages"]
        .iter()
        .all(|required| tables.iter().any(|(name,)| name == required))
    {
        return Err(WorkspaceError::UnsupportedDatabaseSchema.into());
    }
    for (table, required) in [
        (
            "threads",
            &[
                "id",
                "title",
                "cwd",
                "created_at",
                "updated_at",
                "message_count",
            ][..],
        ),
        (
            "messages",
            &["message_id", "thread_id", "role", "content"][..],
        ),
    ] {
        let actual = column_names(connection, table).await?;
        if !required.iter().all(|column| actual.contains(*column)) {
            return Err(WorkspaceError::UnsupportedDatabaseSchema.into());
        }
    }
    Ok(SchemaState::Legacy)
}

async fn column_names(connection: &mut SqliteConnection, table: &str) -> Result<HashSet<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info(?)")
        .bind(table)
        .fetch_all(connection)
        .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

impl SqliteThreadStore {
    /// DDL 与版本号在同一事务中提交；不回填历史 SessionBinding。
    pub(super) async fn init_schema(&self) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let state = inspect(&mut tx).await?;
        if state == SchemaState::Current {
            tx.commit().await?;
            return Ok(());
        }
        if state == SchemaState::Version2 {
            // v2's unused revision column is NOT NULL without a default. Remove
            // it before current writers stop supplying it; all remaining data stays intact.
            sqlx::query("ALTER TABLE session_bindings DROP COLUMN revision")
                .execute(&mut *tx)
                .await?;
        } else if matches!(state, SchemaState::Empty | SchemaState::Legacy) {
            sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS threads (
                id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS messages (
                message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                role TEXT NOT NULL, content TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages(thread_id);"
        ).execute(&mut *tx).await?;
            // 新库与逐步升级的旧库使用同一列定义，已有列及其值保持原样。
            for (table, columns) in [
                (
                    "threads",
                    &[
                        ("parent_thread_id", "TEXT"),
                        ("snapshot_at_message_id", "TEXT"),
                        ("hidden", "BOOLEAN NOT NULL DEFAULT 0"),
                        ("cancel_policy", "TEXT NOT NULL DEFAULT 'cascade'"),
                        ("config", "TEXT"),
                        ("cached_context", "TEXT"),
                        ("frozen_context", "TEXT"),
                        ("inherited_context", "TEXT"),
                        ("agent_status", "TEXT NOT NULL DEFAULT 'active'"),
                        ("context_cache_epoch", "INTEGER NOT NULL DEFAULT 0"),
                    ][..],
                ),
                (
                    "messages",
                    &[
                        ("truncated", "BOOLEAN NOT NULL DEFAULT 0"),
                        ("excluded", "BOOLEAN NOT NULL DEFAULT 0"),
                        ("projection", "TEXT"),
                    ][..],
                ),
            ] {
                let actual = column_names(&mut tx, table).await?;
                for (name, definition) in columns {
                    if !actual.contains(*name) {
                        // 标识符和列定义均来自上方静态 schema，未包含外部输入。
                        sqlx::query(AssertSqlSafe(format!(
                            "ALTER TABLE {table} ADD COLUMN {name} {definition}"
                        )))
                        .execute(&mut *tx)
                        .await?;
                    }
                }
            }
            sqlx::raw_sql(
            "CREATE TABLE projects (
                id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE, object_identity TEXT NOT NULL UNIQUE
            );
            CREATE TABLE workspaces (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
                root TEXT NOT NULL UNIQUE, root_identity TEXT NOT NULL UNIQUE, discovery TEXT NOT NULL,
                UNIQUE(id, project_id)
            );
            CREATE TABLE session_bindings (
                thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
                schema_version INTEGER NOT NULL,
                project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
                FOREIGN KEY(workspace_id, project_id) REFERENCES workspaces(id, project_id)
            );
            CREATE INDEX idx_bindings_project ON session_bindings(project_id, thread_id);
            CREATE INDEX idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id);
            CREATE INDEX idx_threads_updated ON threads(updated_at DESC, id DESC) WHERE hidden = 0 AND message_count > 0;
            CREATE TABLE execution_runs (
                thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
                generation INTEGER NOT NULL, clean BOOLEAN NOT NULL
            );"
        ).execute(&mut *tx).await?;
        }
        if matches!(state, SchemaState::Version2 | SchemaState::Version3) {
            migrate_identity_values(&mut tx).await?;
        }
        sqlx::query("PRAGMA user_version = 4")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

async fn migrate_identity_values(connection: &mut SqliteConnection) -> Result<()> {
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name IN ('projects', 'workspaces')",
    )
    .fetch_all(&mut *connection)
    .await?;
    if tables.len() != 2 {
        return Err(WorkspaceError::UnsupportedDatabaseSchema.into());
    }

    let projects: Vec<(String, String)> =
        sqlx::query_as("SELECT id, object_identity FROM projects ORDER BY id")
            .fetch_all(&mut *connection)
            .await?;
    let mut normalized_projects = Vec::with_capacity(projects.len());
    let mut project_identities = HashSet::new();
    for (id, identity) in projects {
        let value: serde_json::Value = serde_json::from_str(&identity)?;
        let identity = super::discovery::normalize_identity_json(&value)?;
        if !project_identities.insert(identity.clone()) {
            return Err(WorkspaceError::DiscoveryError(
                "object identity collision during schema migration".into(),
            )
            .into());
        }
        normalized_projects.push((id, identity));
    }

    let workspaces: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, root_identity, discovery FROM workspaces ORDER BY id")
            .fetch_all(&mut *connection)
            .await?;
    let mut normalized_workspaces = Vec::with_capacity(workspaces.len());
    let mut workspace_identities = HashSet::new();
    for (id, root_identity, discovery) in workspaces {
        let identity =
            super::discovery::normalize_identity_json(&serde_json::from_str(&root_identity)?)?;
        let discovery =
            super::discovery::normalize_discovery_json(&serde_json::from_str(&discovery)?)?;
        if !workspace_identities.insert(identity.clone()) {
            return Err(WorkspaceError::DiscoveryError(
                "workspace identity collision during schema migration".into(),
            )
            .into());
        }
        normalized_workspaces.push((id, identity, discovery));
    }
    // Validate every row and all collisions before touching unique columns.
    for (id, identity) in normalized_projects {
        sqlx::query("UPDATE projects SET object_identity = ? WHERE id = ?")
            .bind(identity)
            .bind(id)
            .execute(&mut *connection)
            .await?;
    }
    for (id, identity, discovery) in normalized_workspaces {
        sqlx::query("UPDATE workspaces SET root_identity = ?, discovery = ? WHERE id = ?")
            .bind(identity)
            .bind(discovery)
            .bind(id)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "schema_test.rs"]
mod tests;
