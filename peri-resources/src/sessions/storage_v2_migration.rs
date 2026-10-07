//! SQLite schema 11 → 12 的数据搬运。接入写打开前由夹具验证完整形状与回滚。

use anyhow::{bail, Result};
use sqlx::{Connection, SqliteConnection};

use crate::sessions::canonical;
use crate::sessions::storage_v2_plan::read_local_plan;

/// 仅在 schema 11 的写打开中调用；调用方持有初始化锁。迁移前禁用 FK，
/// 完成前检查全部引用，再恢复原设置。任何失败都不推进版本或清理旧凭证。
pub(super) async fn migrate_local_v2(connection: &mut SqliteConnection) -> Result<()> {
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await?;
    if version != 11 {
        bail!("storage v2 migration requires schema 11");
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
    let removals = super::schema_cleanup::removal_plan(&mut tx).await?;
    for statement in removals {
        sqlx::query(statement).execute(&mut *tx).await?;
    }
    // v11 可能来自旧版打开路径，先在本事务补齐环境；后续规划不凭当前 cwd 猜测。
    sqlx::query(canonical::CREATE_ENVIRONMENTS_TABLE_SQL)
        .execute(&mut *tx)
        .await?;
    sqlx::query(canonical::BACKFILL_ENVIRONMENTS_SQL)
        .bind(crate::sessions::machine::current()?)
        .execute(&mut *tx)
        .await?;
    let plan = read_local_plan(&mut tx).await?;

    // 旧 workspaces 保存可变执行登记。保留它的 UUID 和最后观测值，
    // 再将原表名交给 v2 的机器/路径归属；SQLite 同时改写旧 binding 的 FK 目标。
    sqlx::query("ALTER TABLE workspaces RENAME TO legacy_execution_registrations")
        .execute(&mut *tx)
        .await?;
    sqlx::query(canonical::CREATE_V2_MACHINES_TABLE_SQL)
        .execute(&mut *tx)
        .await?;
    sqlx::query(canonical::CREATE_V2_WORKSPACES_TABLE_SQL)
        .execute(&mut *tx)
        .await?;
    let mut machine_ids = std::collections::BTreeSet::new();
    for workspace in &plan.workspaces {
        machine_ids.insert(&workspace.machine_id);
    }
    for machine_id in machine_ids {
        let known =
            uuid::Uuid::parse_str(machine_id).is_ok_and(|parsed| parsed.to_string() == *machine_id);
        sqlx::query("INSERT INTO machines(id, name, identity_kind) VALUES (?1, ?2, ?3)")
            .bind(machine_id)
            .bind(if known { "我的电脑" } else { "旧机器" })
            .bind(if known { "known" } else { "legacy_unknown" })
            .execute(&mut *tx)
            .await?;
    }
    // 当前 Machine 即使尚无 Session 也应可查询。不能覆盖已有展示名。
    sqlx::query(
        "INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, '我的电脑', 'known')",
    )
    .bind(crate::sessions::machine::current()?)
    .execute(&mut *tx)
    .await?;
    for workspace in &plan.workspaces {
        let path = workspace
            .path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 workspace path"))?;
        let source = match workspace.path_source {
            peri_acp_types::workspace::WorkspacePathSource::Discovered => "discovered",
            peri_acp_types::workspace::WorkspacePathSource::DerivedLegacy => "derived_legacy",
            peri_acp_types::workspace::WorkspacePathSource::Unverified => "unverified",
        };
        sqlx::query(
            "INSERT INTO workspaces(id, machine_id, path, path_source) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(workspace.id.to_string())
        .bind(&workspace.machine_id)
        .bind(path)
        .bind(source)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query("ALTER TABLE threads ADD COLUMN workspace_id TEXT REFERENCES workspaces(id)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("ALTER TABLE threads ADD COLUMN archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))")
        .execute(&mut *tx).await?;
    for (thread_id, workspace_id) in &plan.session_workspace_ids {
        sqlx::query("UPDATE threads SET workspace_id = ?2 WHERE id = ?1")
            .bind(thread_id)
            .bind(workspace_id.to_string())
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(canonical::CREATE_V2_INDEXES[4])
        .execute(&mut *tx)
        .await?;

    sqlx::query("ALTER TABLE session_bindings ADD COLUMN discovery_snapshot TEXT")
        .execute(&mut *tx)
        .await?;
    sqlx::query("ALTER TABLE session_bindings ADD COLUMN evidence_origin TEXT NOT NULL DEFAULT 'legacy_last_observation'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE session_bindings SET discovery_snapshot = (SELECT discovery FROM legacy_execution_registrations WHERE id = session_bindings.workspace_id)")
        .execute(&mut *tx)
        .await?;
    let missing: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM session_bindings WHERE discovery_snapshot IS NULL")
            .fetch_one(&mut *tx)
            .await?;
    if missing.0 != 0 {
        bail!("storage v2 execution evidence copy is incomplete");
    }

    // 旧凭证只有 machine 作用域，无法证明属于哪个 Workspace。
    sqlx::query("DROP TABLE mcp_oauth_credentials")
        .execute(&mut *tx)
        .await?;
    sqlx::query(canonical::CREATE_V2_OAUTH_CREDENTIALS_TABLE_SQL)
        .execute(&mut *tx)
        .await?;
    sqlx::query(canonical::CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DROP TABLE session_environments")
        .execute(&mut *tx)
        .await?;
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut *tx)
        .await?;
    if !violations.is_empty() {
        bail!("storage v2 migration broke foreign keys");
    }
    sqlx::query("PRAGMA user_version = 12")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
#[path = "storage_v2_migration_test.rs"]
mod tests;
