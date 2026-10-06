//! 单库 schema 升级：保留历史与会话身份，事务内清理退役状态。

use super::database::SqliteSessionDatabase;
#[cfg(test)]
use super::SqliteThreadStore;
use crate::sessions::canonical::{self, CREATE_INDEXES, CREATE_TABLES};
use anyhow::Result;
use peri_acp_types::workspace::WorkspaceError;
use sqlx::{AssertSqlSafe, Connection, SqliteConnection};
use std::collections::HashSet;

/// 本机与远端使用同一会话 schema 版本。
pub(in crate::sessions) use crate::sessions::canonical::CURRENT_SCHEMA_VERSION;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SchemaState {
    Empty,
    Legacy,
    Version2,
    Version3,
    Version4,
    Version5,
    Version6,
    Version7,
    Version8,
    Version9,
    Version10,
    Version11,
    Version12,
    Version13,
    Version14,
    Version15,
    Version16,
    LegacyWork17,
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
        17 => {
            let old_work_table: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'session_work_state')",
            )
            .fetch_one(&mut *connection)
            .await?;
            return Ok(if old_work_table {
                SchemaState::LegacyWork17
            } else {
                SchemaState::Current
            });
        }
        16 => return Ok(SchemaState::Version16),
        15 => return Ok(SchemaState::Version15),
        14 => return Ok(SchemaState::Version14),
        13 => return Ok(SchemaState::Version13),
        12 => return Ok(SchemaState::Version12),
        11 => return Ok(SchemaState::Version11),
        10 => return Ok(SchemaState::Version10),
        9 => return Ok(SchemaState::Version9),
        8 => return Ok(SchemaState::Version8),
        7 => return Ok(SchemaState::Version7),
        6 => return Ok(SchemaState::Version6),
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

/// 升级路径的同名预检：canonical 表名上若已经立着**别的类型**的对象（例如同名 VIEW），
/// `CREATE TABLE IF NOT EXISTS` 会静默跳过，把一个不是 canonical 形状的对象留在表的位置上。
/// 这里显式拒绝，保持迁移的 fail-closed（与 v10 删除旧表前的形状校验同一思路）。
///
/// 只覆盖表名（DDL 里的索引名同样是 `IF NOT EXISTS`，同名索引冲突不在本预检范围内）。
/// 错误文案带上对象名与它的实际类型：报错要能回答「为什么不支持」。
async fn ensure_canonical_names_hold_tables(connection: &mut SqliteConnection) -> Result<()> {
    for table in canonical::CANONICAL_TABLES {
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT type FROM sqlite_schema WHERE name = ?1")
                .bind(table)
                .fetch_optional(&mut *connection)
                .await?;
        if let Some((actual_type,)) = existing {
            if actual_type != "table" {
                anyhow::bail!("schema upgrade blocked: {table} already exists as a {actual_type}");
            }
        }
    }
    Ok(())
}

async fn column_names(connection: &mut SqliteConnection, table: &str) -> Result<HashSet<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info(?)")
        .bind(table)
        .fetch_all(connection)
        .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

impl SqliteSessionDatabase {
    /// DDL 与版本号在同一事务中提交；不回填历史 SessionBinding。
    pub(super) async fn init_schema(&self) -> Result<()> {
        crate::sessions::machine::initialize().await?;
        let mut connection = self.pool.acquire().await?;
        let state = inspect(&mut connection).await?;
        if state == SchemaState::Current {
            sqlx::query(canonical::CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL)
                .execute(&mut *connection)
                .await?;
            sqlx::query("INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, '我的电脑', 'known')")
                .bind(crate::sessions::machine::current()?)
                .execute(&mut *connection)
                .await?;
            return Ok(());
        }
        if state != SchemaState::Empty {
            anyhow::bail!("legacy work schema 17 requires explicit stopped-writer offline migration; opening a legacy database never migrates it");
        }
        Self::initialize_work_schema(&mut connection).await
    }

    async fn initialize_work_schema(connection: &mut SqliteConnection) -> Result<()> {
        let mut tx = connection.begin_with("BEGIN IMMEDIATE").await?;
        for statement in canonical::CREATE_V2_TABLES
            .iter()
            .chain(canonical::CREATE_V2_INDEXES)
        {
            sqlx::query(*statement).execute(&mut *tx).await?;
        }
        sqlx::query(crate::sessions::control::CREATE_STATE)
            .execute(&mut *tx)
            .await?;
        sqlx::query(crate::sessions::control::CREATE_RECEIPTS)
            .execute(&mut *tx)
            .await?;
        sqlx::query(crate::sessions::control::SEED_STATE)
            .execute(&mut *tx)
            .await?;
        for statement in crate::sessions::work_store::schema::initialization_sql() {
            sqlx::query(statement).execute(&mut *tx).await?;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO machines(id,name,identity_kind) VALUES (?1,'我的电脑','known')",
        )
        .bind(crate::sessions::machine::current()?)
        .execute(&mut *tx)
        .await?;
        sqlx::query("PRAGMA user_version = 17")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn migrate_schema(connection: &mut SqliteConnection, state: SchemaState) -> Result<()> {
        let mut tx = connection.begin_with("BEGIN IMMEDIATE").await?;
        let removals = super::schema_cleanup::removal_plan(&mut tx).await?;
        if state == SchemaState::Version2 {
            // v2's unused revision column is NOT NULL without a default. Remove
            // it before current writers stop supplying it; all remaining data stays intact.
            sqlx::query("ALTER TABLE session_bindings DROP COLUMN revision")
                .execute(&mut *tx)
                .await?;
        } else if matches!(state, SchemaState::Empty | SchemaState::Legacy) {
            // 建表语句来自 `sessions::canonical`：本机新库与远端初始化下发的是同一份清单
            // （逐条执行，两边执行器的语句单元相同），形状不可能各自漂移。旧库（表已存在）
            // 在这里是空操作，列由下面的补列循环补齐。
            ensure_canonical_names_hold_tables(&mut tx).await?;
            // 标识符与列定义均来自 `canonical` 的静态清单，不含外部输入。
            for statement in CREATE_TABLES {
                sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
                    .execute(&mut *tx)
                    .await?;
            }
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
                        ("frozen_context", "TEXT"),
                        ("inherited_context", "TEXT"),
                        ("agent_status", "TEXT NOT NULL DEFAULT 'active'"),
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
            // 索引在建表与补列之后：`idx_threads_updated` 引用后补的列。
            for statement in CREATE_INDEXES {
                sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
                    .execute(&mut *tx)
                    .await?;
            }
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
        // v10 回退：删除 v7..v9 写下的本机远程痕迹（本机登记、未决锚点、远端操作日志、
        // 按 store 分区的执行域）。对没有这些表的库是幂等的。
        drop_remote_local_state(&mut tx).await?;
        for statement in removals {
            sqlx::query(statement).execute(&mut *tx).await?;
        }
        sqlx::query(AssertSqlSafe(canonical::CREATE_OAUTH_CREDENTIALS_TABLE_SQL))
            .execute(&mut *tx)
            .await?;
        // 环境事实与 schema 版本同一事务提交。未来的 Machine/Workspace 归属回填
        // 必须能使用这些行，不能先留下已推进版本而缺少环境归属的半升级库。
        sqlx::query(AssertSqlSafe(canonical::CREATE_ENVIRONMENTS_TABLE_SQL))
            .execute(&mut *tx)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_session_environments_machine ON session_environments(machine_id, thread_id)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(AssertSqlSafe(canonical::BACKFILL_ENVIRONMENTS_SQL))
            .bind(crate::sessions::machine::current()?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("PRAGMA user_version = 11")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// v10 回退：v7..v9 写下的本机远程痕迹一次性删除。
///
/// 用户裁决撤销了「远程存储在本机留有痕迹」的整套能力（只做初始化时的切换，不做交叉
/// 能力），因此这五张表连同它们的语义一起消失，本地表结构回到 remote 工作之前：不加表、
/// 不加列、没有 store 维度。表清单与建表时的列形状一一对应：
///
/// | 表 | 原来承载的事实 |
/// | --- | --- |
/// | `session_store_registrations` | 本机对远端 store 的登记（执行准入依据） |
/// | `session_lifecycle_commitments` | 本机未决写 / 删除墓碑锚点 |
/// | `session_remote_operations` | 远端操作日志（发送前登记与结清） |
/// | `remote_execution_runs` | 按 `(store, root)` 分区的执行代际 |
/// | `remote_lifecycle_commitments` | 按 `(store, thread_id)` 分区的删除锚点 |
///
/// **删除前先校验形状**：表存在但列不符说明它不是本模块建的表（同名的别的业务表），
/// 此时**不删**并拒绝升级（fail-closed，整个事务回滚），而不是把不认识的数据丢掉。
/// 表不存在时跳过，因此对没有这些表的库是幂等的。
///
/// 不触碰 `threads` / `messages` / `session_bindings` / `workspaces` / `projects`。
async fn drop_remote_local_state(connection: &mut SqliteConnection) -> Result<()> {
    for (table, columns) in DROPPED_LOCAL_TABLES {
        if !table_exists(connection, table).await? {
            continue;
        }
        require_columns(connection, table, columns).await?;
        // 标识符来自下方静态清单，未包含外部输入。
        sqlx::query(AssertSqlSafe(format!("DROP TABLE {table}")))
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// v10 删除的本机表及其建表时的列形状。
///
/// 清单只服务一次回退迁移：v10 跑完这些表不复存在，也没有任何写入路径再引用它们，
/// 因此常量本身可以在下一个 schema 版本里一并删除。
const DROPPED_LOCAL_TABLES: &[(&str, &[&str])] = &[
    (
        "session_store_registrations",
        &[
            "store_id",
            "engine",
            "locator_digest",
            "installation_id",
            "created_at",
        ],
    ),
    (
        "session_lifecycle_commitments",
        &[
            "thread_id",
            "root_id",
            "kind",
            "state",
            "generation",
            "operation_id",
            "detail",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "session_remote_operations",
        &[
            "operation_id",
            "store_id",
            "thread_id",
            "root_id",
            "behavior",
            "digest",
            "state",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "remote_execution_runs",
        &["store_id", "root_id", "generation", "clean"],
    ),
    (
        "remote_lifecycle_commitments",
        &[
            "store_id",
            "thread_id",
            "kind",
            "state",
            "created_at",
            "updated_at",
        ],
    ),
];

async fn table_exists(connection: &mut SqliteConnection, table: &str) -> Result<bool> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_schema WHERE type = 'table' AND name = ?")
            .bind(table)
            .fetch_optional(&mut *connection)
            .await?;
    Ok(row.is_some())
}

async fn require_columns(
    connection: &mut SqliteConnection,
    table: &str,
    required: &[&str],
) -> Result<()> {
    let actual = column_names(connection, table).await?;
    if !required.iter().all(|column| actual.contains(*column)) {
        return Err(WorkspaceError::UnsupportedDatabaseSchema.into());
    }
    Ok(())
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
