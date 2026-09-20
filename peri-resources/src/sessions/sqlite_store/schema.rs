//! 单库 schema 升级：保留历史与执行状态，事务内调整结构。

use super::SqliteThreadStore;
use anyhow::Result;
use peri_acp_types::workspace::WorkspaceError;
use sqlx::{AssertSqlSafe, Connection, SqliteConnection};
use std::collections::HashSet;

/// 本构建写入并接受的 schema 版本；2..5 经升级路径收敛到此值，0 视为待建库。
/// 版本接受判定、迁移收尾写入与拒绝时的「本构建上限」都由它派生，避免三处各写一份。
pub(super) const CURRENT_SCHEMA_VERSION: i64 = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SchemaState {
    Empty,
    Legacy,
    Version2,
    Version3,
    Version4,
    Version5,
    Current,
}

impl SchemaState {
    fn needs_registration_rebuild(self) -> bool {
        matches!(
            self,
            Self::Version2 | Self::Version3 | Self::Version4 | Self::Version5
        )
    }
}

/// 旧版未设置 user_version；校验本模块所需基础表，保留同库的其他业务表。
pub(super) async fn inspect(connection: &mut SqliteConnection) -> Result<SchemaState> {
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await?;
    match version {
        v if v == CURRENT_SCHEMA_VERSION => return Ok(SchemaState::Current),
        5 => return Ok(SchemaState::Version5),
        4 => return Ok(SchemaState::Version4),
        3 => return Ok(SchemaState::Version3),
        2 => return Ok(SchemaState::Version2),
        0 => {}
        // 拒绝时复述实际版本：报错要能回答「为什么不支持」，而不是只给结论。
        other => {
            return Err(WorkspaceError::UnsupportedSchemaVersion {
                found: other,
                supported: CURRENT_SCHEMA_VERSION,
            }
            .into());
        }
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
        let mut connection = self.pool.acquire().await?;
        let state = inspect(&mut connection).await?;
        if state == SchemaState::Current {
            return Ok(());
        }
        // 登记表重建要对被引用的父表执行 DROP TABLE：SQLite 对父表做隐式删除时会
        // 立即检查外键，`defer_foreign_keys` 也挡不住。该 PRAGMA 只在事务外生效，
        // 因此重建路径整段使用同一条连接：先关外键，提交前用 foreign_key_check 补齐
        // 校验，最后恢复连接设置。
        let rebuilding = state.needs_registration_rebuild();
        if rebuilding {
            sqlx::query("PRAGMA foreign_keys = OFF")
                .execute(&mut *connection)
                .await?;
        }
        let migrated = Self::migrate_schema(&mut connection, state).await;
        if rebuilding {
            let restored = sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&mut *connection)
                .await;
            migrated?;
            restored?;
        } else {
            migrated?;
        }
        Ok(())
    }

    async fn migrate_schema(connection: &mut SqliteConnection, state: SchemaState) -> Result<()> {
        let mut tx = connection.begin_with("BEGIN IMMEDIATE").await?;
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
                id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
                UNIQUE(locator, object_identity)
            );
            CREATE TABLE workspaces (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
                root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
                UNIQUE(root, root_identity), UNIQUE(id, project_id)
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
        // 2/3 直升也必须完成登记键迁移；5 既可能已放宽，也可能被旧 writer
        // 漏迁移后误标。统一重建一次，保留健康 5 已有的所有组合登记。
        if state.needs_registration_rebuild() {
            relax_registration_keys(&mut tx).await?;
            // 本次迁移关闭了外键强制，提交前显式补齐引用校验。
            let violations: Vec<(String, i64, String, i64)> =
                sqlx::query_as("PRAGMA foreign_key_check")
                    .fetch_all(&mut *tx)
                    .await?;
            if !violations.is_empty() {
                return Err(WorkspaceError::DiscoveryError(
                    "registration rebuild broke references".into(),
                )
                .into());
            }
        }
        sqlx::query(AssertSqlSafe(format!(
            "PRAGMA user_version = {CURRENT_SCHEMA_VERSION}"
        )))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// schema 5：登记键从单列唯一放宽为 (定位路径, 文件对象证据) 组合。
///
/// 目录被替换（同路径的新文件对象）或换位（同一对象的新路径）是正常演进：它们
/// 应当得到新的项目与工作区登记，而不是被单列唯一约束挡成不可登记。放宽只影响
/// 唯一性判定，不涉及行内容——同一组合仍然唯一，旧登记、旧绑定与执行状态保持原样。
async fn relax_registration_keys(connection: &mut SqliteConnection) -> Result<()> {
    let before = registration_counts(connection).await?;
    // 两个表互为引用，调用方已为本次重建关闭外键强制并在提交前做 foreign_key_check；
    // 复制必须逐列进行，行内容与引用关系都保持原样。
    sqlx::raw_sql(
        "CREATE TABLE projects_relaxed (
            id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
            UNIQUE(locator, object_identity)
        );
        INSERT INTO projects_relaxed (id, locator, object_identity)
            SELECT id, locator, object_identity FROM projects;
        DROP TABLE projects;
        ALTER TABLE projects_relaxed RENAME TO projects;
        CREATE TABLE workspaces_relaxed (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
            root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
            UNIQUE(root, root_identity), UNIQUE(id, project_id)
        );
        INSERT INTO workspaces_relaxed (id, project_id, root, root_identity, discovery)
            SELECT id, project_id, root, root_identity, discovery FROM workspaces;
        DROP TABLE workspaces;
        ALTER TABLE workspaces_relaxed RENAME TO workspaces;",
    )
    .execute(&mut *connection)
    .await?;
    if registration_counts(connection).await? != before {
        return Err(WorkspaceError::DiscoveryError(
            "registration rows changed during schema migration".into(),
        )
        .into());
    }
    Ok(())
}

async fn registration_counts(connection: &mut SqliteConnection) -> Result<(i64, i64)> {
    let (projects,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM projects")
        .fetch_one(&mut *connection)
        .await?;
    let (workspaces,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM workspaces")
        .fetch_one(&mut *connection)
        .await?;
    Ok((projects, workspaces))
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
