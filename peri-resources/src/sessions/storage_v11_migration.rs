//! 本机 schema 的压缩后单条迁移：压缩前形状（`V10`，正式基线 ≤10）→ 当前形状（11）。
//!
//! 两种输入都走**同一条**搬运：正式发布的 ≤10 库（调用方先在本事务补齐到 V10 形状）与
//! β 内部停在 11 的登记形状库（形状已经是 V10）。搬运把登记的 `workspaces` 与引用它的
//! 绑定表整体换成当前形状：
//!
//! - 归属行按 `(machine_id, path)` 收敛，id 沿用旧登记 UUID（空闲时），旧登记的证据三列
//!   随行搬过去；没有会话引用但登记仍存在的路径也保留一行（否则登记行会凭空消失）。
//! - 绑定行的 `workspace_id` 从「执行登记 id」收敛到**会话归属行 id**，证据取旧登记的最后
//!   观测（`legacy_last_observation`）。任一绑定落不到归属行、或其记录的根与归属行不同路径，
//!   都说明改写会静默改绑：直接拒绝升级，库保持原版本。
//! - 旧凭证只有 machine 作用域，无法证明 workspace 归属，不搬运（按当前形状重建）。
//! - 压缩前独有的 `session_environments` 在搬运后删除。
//!
//! 整段在一个事务里完成，提交前用 `foreign_key_check` 补齐校验，失败即回滚。
//! 外键强制由调用方在**事务外**关闭（`PRAGMA foreign_keys` 在事务内是空操作）。

use std::collections::BTreeSet;

use anyhow::{bail, Result};
use sqlx::{Connection, SqliteConnection};

use crate::sessions::canonical;
use crate::sessions::storage_v2_plan::{
    machine_identity, path_source_name, plan_binding_rows, plan_workspace_rows, read_local_plan,
    LegacyBindingRow,
};

/// 独立入口：库停在 11 且形状是压缩前的登记形状时调用（调用方持有初始化锁）。
pub(super) async fn migrate_local_v11(connection: &mut SqliteConnection) -> Result<()> {
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await?;
    if version != canonical::CURRENT_SCHEMA_VERSION {
        bail!("storage v11 migration requires schema 11");
    }
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
    migrate_shape(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// 搬运本体（调用方已关闭外键强制，且已在事务内）：清掉退役对象 → 补齐环境事实 →
/// 读计划与旧绑定 → 重建归属与绑定 → 删掉压缩前独有的对象 → 推进版本。返回时
/// `user_version` 已是当前版本。
pub(super) async fn migrate_shape(connection: &mut SqliteConnection) -> Result<()> {
    // 退役对象先清：形状判定的失败要发生在任何改写之前（库保持原版本，旧二进制仍可打开）。
    // ≤10 路径的调用方在本事务开头已跑过一次，第二次是幂等的空操作。
    for statement in super::schema_cleanup::removal_plan(connection).await? {
        // 语句文本来自 `schema_cleanup` 的静态清单，不含外部输入。
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&mut *connection)
            .await?;
    }
    // 环境事实是规划输入：老库可能缺行（旧版的打开路径不写它），先在本事务补齐。
    sqlx::query(canonical::V10_CREATE_ENVIRONMENTS_TABLE_SQL)
        .execute(&mut *connection)
        .await?;
    sqlx::query(canonical::BACKFILL_ENVIRONMENTS_SQL)
        .bind(crate::sessions::machine::current()?)
        .execute(&mut *connection)
        .await?;
    let input = read_local_plan(&mut *connection).await?;
    let bindings = read_legacy_bindings(&mut *connection).await?;

    // 旧登记与引用它的绑定表整体换掉：SQLite 不能删约束，只能重建，
    // 而重建同时把两个 id 空间收敛成一个。
    sqlx::query("DROP TABLE session_bindings")
        .execute(&mut *connection)
        .await?;
    sqlx::query("DROP TABLE workspaces")
        .execute(&mut *connection)
        .await?;

    // 当前 Machine 即使尚无 Session 也应可查询；不能覆盖已有展示名。
    let mut machines: BTreeSet<String> = input
        .plan
        .workspaces
        .iter()
        .map(|workspace| workspace.machine_id.clone())
        .collect();
    machines.insert(crate::sessions::machine::current()?.to_owned());
    sqlx::query(canonical::CREATE_MACHINES_TABLE_SQL)
        .execute(&mut *connection)
        .await?;
    for machine_id in &machines {
        let (name, kind) = machine_identity(machine_id);
        sqlx::query("INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, ?2, ?3)")
            .bind(machine_id)
            .bind(name)
            .bind(kind)
            .execute(&mut *connection)
            .await?;
    }
    sqlx::query(canonical::CREATE_WORKSPACES_TABLE_SQL)
        .execute(&mut *connection)
        .await?;
    for row in plan_workspace_rows(
        &input.plan,
        &input.sessions,
        &input.registrations,
        &machines,
    )? {
        let path = row
            .path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 workspace path"))?;
        sqlx::query(
            "INSERT INTO workspaces(id, machine_id, path, path_source, project_id, identity, discovery)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(row.id.to_string())
        .bind(&row.machine_id)
        .bind(path)
        .bind(path_source_name(row.path_source))
        .bind(&row.project_id)
        .bind(&row.identity)
        .bind(&row.discovery)
        .execute(&mut *connection)
        .await?;
    }

    sqlx::query("ALTER TABLE threads ADD COLUMN workspace_id TEXT REFERENCES workspaces(id)")
        .execute(&mut *connection)
        .await?;
    sqlx::query(
        "ALTER TABLE threads ADD COLUMN archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))",
    )
    .execute(&mut *connection)
    .await?;
    for (thread_id, workspace_id) in &input.plan.session_workspace_ids {
        sqlx::query("UPDATE threads SET workspace_id = ?2 WHERE id = ?1")
            .bind(thread_id)
            .bind(workspace_id.to_string())
            .execute(&mut *connection)
            .await?;
    }
    // 归属索引引用后补的列，必须在补列与回填之后建。
    sqlx::query(canonical::THREAD_WORKSPACE_INDEX)
        .execute(&mut *connection)
        .await?;

    sqlx::query(canonical::CREATE_BINDINGS_TABLE_SQL)
        .execute(&mut *connection)
        .await?;
    for row in plan_binding_rows(&bindings, &input.plan, &input.registrations)? {
        sqlx::query(
            "INSERT INTO session_bindings(thread_id, schema_version, project_id, workspace_id, relative_cwd, discovery_snapshot, evidence_origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&row.thread_id)
        .bind(row.schema_version)
        .bind(&row.project_id)
        .bind(row.workspace_id.to_string())
        .bind(&row.relative_cwd)
        .bind(&row.discovery_snapshot)
        .bind(row.evidence_origin)
        .execute(&mut *connection)
        .await?;
    }
    for statement in canonical::BINDING_INDEXES {
        // 语句文本来自 `canonical` 的静态清单，不含外部输入。
        sqlx::query(sqlx::AssertSqlSafe(*statement))
            .execute(&mut *connection)
            .await?;
    }

    // 旧凭证只有 machine 作用域，无法证明属于哪个 Workspace。
    sqlx::query("DROP TABLE mcp_oauth_credentials")
        .execute(&mut *connection)
        .await?;
    sqlx::query(canonical::CREATE_OAUTH_CREDENTIALS_TABLE_SQL)
        .execute(&mut *connection)
        .await?;
    sqlx::query(canonical::CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL)
        .execute(&mut *connection)
        .await?;
    // 环境事实与归属行合并之后，压缩前独有的这张表不再承载任何事实。
    sqlx::query("DROP TABLE session_environments")
        .execute(&mut *connection)
        .await?;

    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut *connection)
        .await?;
    if !violations.is_empty() {
        bail!("storage v11 migration broke foreign keys");
    }
    sqlx::query("PRAGMA user_version = 11")
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// 旧绑定行：`workspace_id` 指向执行登记，不是归属行。
async fn read_legacy_bindings(connection: &mut SqliteConnection) -> Result<Vec<LegacyBindingRow>> {
    let rows: Vec<(String, i64, String, String, String)> = sqlx::query_as(
        "SELECT thread_id, schema_version, project_id, workspace_id, relative_cwd FROM session_bindings",
    )
    .fetch_all(connection)
    .await?;
    rows.into_iter()
        .map(
            |(thread_id, schema_version, project_id, workspace_id, relative_cwd)| {
                Ok(LegacyBindingRow {
                    thread_id,
                    schema_version,
                    project_id,
                    workspace_id: workspace_id.parse()?,
                    relative_cwd,
                })
            },
        )
        .collect()
}

#[cfg(test)]
#[path = "storage_v11_migration_test.rs"]
mod tests;
