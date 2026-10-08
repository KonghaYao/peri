//! SQLite schema 18 → 19：执行登记并入 `workspaces`，绑定表去掉指向登记表的外键。
//!
//! 归属权威从「登记行」收敛到 `workspaces` 的 `(machine_id, path)`：登记的
//! `project_id`/`root_identity`/`discovery` 三个值落在该路径的归属行上；绑定行的
//! `workspace_id` 由执行登记 id 收敛到**会话归属行 id**（`threads.workspace_id`），
//! 其余字节（相对路径、`discovery_snapshot`、`evidence_origin`）原样搬运。
//!
//! 为什么要收敛绑定：v19 之前两个 id 空间并存——归属行 id 由 `(machine_id, path)` 决定，
//! 执行登记 id 由 `(root, root_identity)` 决定。二者只在本机老库上恰好相同（v12 计划沿用
//! 旧 UUID）；v12 之后新建的目录两者是两个独立 UUID，绑定指向的登记 id 在归属表里根本
//! 没有对应行。删除登记表必须同时把绑定搬到归属行，否则这些绑定会失去归属。
//!
//! 搬迁前先验证「绑定记录的根 = 会话归属行路径」（两条计数校验），因此搬运是**保义**的：
//! 同一台机器、同一路径、同一项目、同一相对路径，只有 id 空间被收敛。
//!
//! 删除父表会触发外键检查，因此整段迁移在 `PRAGMA foreign_keys = OFF` 下执行，
//! 提交前用 `foreign_key_check` 补齐校验，最后恢复连接设置（与 11 → 12 同一模式）。
//! 任何失败都不推进版本：库保持 18，可由上一版二进制继续打开。

use anyhow::{bail, Result};
use sqlx::{Connection, SqliteConnection};

use crate::sessions::canonical;

/// 登记表在 v19 之前的列形状；同名的别的表一律拒绝升级，而不是把不认识的数据丢掉。
const REGISTRATION_COLUMNS: &[&str] = &["id", "project_id", "root", "root_identity", "discovery"];

/// 仅在 schema 18 的写打开中调用；调用方持有初始化锁。
pub(super) async fn migrate_local_v19(connection: &mut SqliteConnection) -> Result<()> {
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await?;
    if version != 18 {
        bail!("storage v19 migration requires schema 18");
    }
    require_registrations_shape(connection).await?;
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await?;
    let migrated = migrate_transaction(connection).await;
    let restored = sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await;
    migrated?;
    restored?;
    Ok(())
}

async fn migrate_transaction(connection: &mut SqliteConnection) -> Result<()> {
    let mut tx = connection.begin_with("BEGIN IMMEDIATE").await?;
    let columns = column_names(&mut tx, "workspaces").await?;
    for (name, statement) in [
        ("project_id", canonical::ADD_WORKSPACES_PROJECT_COLUMN_SQL),
        ("identity", canonical::ADD_WORKSPACES_IDENTITY_COLUMN_SQL),
        ("discovery", canonical::ADD_WORKSPACES_DISCOVERY_COLUMN_SQL),
    ] {
        if !columns.contains(name) {
            // 标识符与列定义均来自 `canonical` 的静态清单，不含外部输入。
            sqlx::query(statement).execute(&mut *tx).await?;
        }
    }
    sqlx::query(canonical::BACKFILL_WORKSPACES_EVIDENCE_SQL)
        .execute(&mut *tx)
        .await?;
    sqlx::query(canonical::BACKFILL_MISSING_WORKSPACES_SQL)
        .execute(&mut *tx)
        .await?;
    // 收敛绑定之前先证明它是保义的：每条绑定都要落得到会话归属行，且绑定自己记录的根
    // 就是该归属行的路径。任何一条不成立都说明改写会静默改绑（或丢绑定），按 fail-closed
    // 拒绝升级，库留在 18 由上一版二进制继续打开。
    for (statement, message) in [
        (
            canonical::COUNT_BINDINGS_WITHOUT_OWNER_SQL,
            "storage v19 migration found bindings without a session workspace owner",
        ),
        (
            canonical::COUNT_BINDINGS_AT_FOREIGN_ROOT_SQL,
            "storage v19 migration found bindings recorded at a different root than their session workspace",
        ),
    ] {
        let (mismatched,): (i64,) = sqlx::query_as(statement).fetch_one(&mut *tx).await?;
        if mismatched != 0 {
            bail!("{message}");
        }
    }
    for statement in canonical::REBUILD_BINDINGS_WITHOUT_REGISTRATIONS_SQL {
        // 语句文本来自 `canonical` 的静态清单，不含外部输入。
        sqlx::query(sqlx::AssertSqlSafe(*statement))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(canonical::DROP_LEGACY_REGISTRATIONS_SQL)
        .execute(&mut *tx)
        .await?;
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut *tx)
        .await?;
    if !violations.is_empty() {
        bail!("storage v19 migration broke foreign keys");
    }
    sqlx::query("PRAGMA user_version = 19")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// 登记表必须存在且是 v18 的已知形状。
async fn require_registrations_shape(connection: &mut SqliteConnection) -> Result<()> {
    let kind: Option<(String,)> = sqlx::query_as(
        "SELECT type FROM sqlite_master WHERE name = 'legacy_execution_registrations'",
    )
    .fetch_optional(&mut *connection)
    .await?;
    let Some((kind,)) = kind else {
        bail!("storage v19 migration requires the execution registration table");
    };
    if kind != "table" {
        bail!("schema upgrade blocked: legacy_execution_registrations is a {kind}");
    }
    let actual = column_names(connection, "legacy_execution_registrations").await?;
    if !REGISTRATION_COLUMNS
        .iter()
        .all(|column| actual.contains(*column))
    {
        bail!("unrecognized execution registration table");
    }
    Ok(())
}

async fn column_names(
    connection: &mut SqliteConnection,
    table: &str,
) -> Result<std::collections::HashSet<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info(?)")
        .bind(table)
        .fetch_all(connection)
        .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

#[cfg(test)]
#[path = "storage_v19_migration_test.rs"]
mod tests;
