//! Guarded remote schema 18 → 19 upgrade merging execution registrations into `workspaces`.
//!
//! 远端与本地共用同一批迁移语句（`sessions::canonical`），差别只在执行器：远端把整段
//! 迁移作为**一次受管批次**下发，本机在事务内逐条执行。这里因此不重写迁移语义，只补两样
//! 远端特有的东西：
//!
//! - **守卫**：形状快照、逐对象定义、两条搬运前置计数插在批次最前，任何一条不成立都让
//!   批次以唯一键冲突失败——读到形状与提交时形状必须一致，迁移不作用在漂移过的库上。
//!   本机在事务内读回计数再判定，远端没有事务内读回，只能把同一条规则换成本批次的载体
//!   （`INSERT ... SELECT 0 WHERE (<计数>) <> 0`），规则文本仍是 `canonical` 那一份。
//! - **版本与契约推进**：作为批次最后一条，同时把契约从 [`PREVIOUS_STORE_CONTRACT`] 推进到
//!   [`STORE_CONTRACT`]——形状变了，代数跟着变，旧构建凭契约标签拒绝这个库。

use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use turso_serverless::Value;

use super::{
    mutation::RemoteStore,
    schema::{self, StoreSnapshot},
    sql::{text_at, StatementSpec},
};
use crate::sessions::{canonical, schema_cleanup::SchemaObject};

/// 迁移前的登记表列形状；缺列说明这不是 v18 的登记表，拒绝而不是按猜测搬运。
const REGISTRATION_COLUMNS: &[&str] = &["id", "project_id", "root", "root_identity", "discovery"];
const READ_REGISTRATION_COLUMNS_SQL: &str =
    "SELECT name FROM pragma_table_info('legacy_execution_registrations') ORDER BY cid";
const READ_WORKSPACE_COLUMNS_SQL: &str =
    "SELECT name FROM pragma_table_info('workspaces') ORDER BY cid";
/// 本次迁移补到归属行上的证据列。
const ADDED_WORKSPACE_COLUMNS: &[&str] = &["project_id", "identity", "discovery"];

/// 搬运前置计数 1 的批次载体（规则文本与 [`canonical::COUNT_BINDINGS_WITHOUT_OWNER_SQL`] 相同）。
const GUARD_BINDINGS_WITHOUT_OWNER_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE (SELECT COUNT(*) FROM session_bindings b
    LEFT JOIN threads t ON t.id = b.thread_id
    LEFT JOIN workspaces owner ON owner.id = t.workspace_id
    WHERE t.id IS NULL OR owner.id IS NULL) <> 0";
/// 搬运前置计数 2 的批次载体（规则文本与 [`canonical::COUNT_BINDINGS_AT_FOREIGN_ROOT_SQL`] 相同）。
const GUARD_BINDINGS_AT_FOREIGN_ROOT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE (SELECT COUNT(*) FROM session_bindings b
    LEFT JOIN legacy_execution_registrations r ON r.id = b.workspace_id
    LEFT JOIN workspaces recorded ON recorded.id = b.workspace_id
    JOIN threads t ON t.id = b.thread_id
    JOIN workspaces owner ON owner.id = t.workspace_id
    WHERE COALESCE(r.root, recorded.path) IS NULL
       OR COALESCE(r.root, recorded.path) <> owner.path) <> 0";
const ADVANCE_VERSION_SQL: &str = "UPDATE peri_store_meta SET schema_version = ?1, contract = ?2
    WHERE singleton = 0 AND schema_version = 18 AND store_id = ?3 AND contract = ?4";

fn unsupported() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Unsupported)
}

pub(super) async fn upgrade(
    store: &RemoteStore,
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<()> {
    if snapshot.schema_version != 18 || snapshot.contract != schema::PREVIOUS_STORE_CONTRACT {
        return Err(unsupported());
    }
    let results = store
        .read_batch(vec![
            StatementSpec::bare(crate::sessions::schema_cleanup::SCHEMA_OBJECTS_SQL),
            StatementSpec::bare(READ_REGISTRATION_COLUMNS_SQL),
            StatementSpec::bare(READ_WORKSPACE_COLUMNS_SQL),
        ])
        .await?;
    let objects = super::schema_upgrade::decode_objects(&results[0])?;
    let registrations = column_names(&results[1])?;
    let workspaces = column_names(&results[2])?;
    store
        .apply_schema_upgrade(upgrade_plan(
            snapshot,
            &objects,
            &registrations,
            &workspaces,
        )?)
        .await
}

/// 从一次一致快照构造迁移批次（纯函数，离线断言语句顺序与拒绝形状）。
///
/// 补列语句按快照决定是否下发：v18 的归属行没有证据列，本次迁移补上；由 v12 迁移用当前
/// `canonical` 常量建出的库已经带着这三列，此时跳过（远端没有 `ADD COLUMN IF NOT EXISTS`，
/// 只能按快照构造条件批次——读到形状与提交时形状的一致由守卫保证）。只存在一部分说明这
/// 不是本构建认识的形状，拒绝而不是猜。
pub(super) fn upgrade_plan(
    snapshot: &StoreSnapshot,
    objects: &[SchemaObject],
    registrations: &[String],
    workspaces: &[String],
) -> SessionResourceResult<Vec<StatementSpec>> {
    if snapshot.schema_version != 18 || snapshot.contract != schema::PREVIOUS_STORE_CONTRACT {
        return Err(unsupported());
    }
    if !REGISTRATION_COLUMNS.iter().all(|column| {
        registrations
            .iter()
            .any(|actual| actual.eq_ignore_ascii_case(column))
    }) {
        return Err(unsupported());
    }
    let present = ADDED_WORKSPACE_COLUMNS
        .iter()
        .filter(|column| {
            workspaces
                .iter()
                .any(|actual| actual.eq_ignore_ascii_case(column))
        })
        .count();
    let add_columns = match present {
        0 => true,
        count if count == ADDED_WORKSPACE_COLUMNS.len() => false,
        _ => return Err(unsupported()),
    };
    let mut plan = vec![StatementSpec::new(
        super::schema_upgrade::GUARD_SNAPSHOT_SQL,
        vec![
            Value::Integer(objects.len() as i64),
            Value::Integer(snapshot.schema_version),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    )];
    for object in objects {
        plan.push(StatementSpec::new(
            super::schema_upgrade::GUARD_OBJECT_SQL,
            vec![
                Value::Text(object.kind.clone()),
                Value::Text(object.name.clone()),
                Value::Text(object.table.clone()),
                object.sql.clone().map(Value::Text).unwrap_or(Value::Null),
            ],
        ));
    }
    plan.push(StatementSpec::bare(GUARD_BINDINGS_WITHOUT_OWNER_SQL));
    plan.push(StatementSpec::bare(GUARD_BINDINGS_AT_FOREIGN_ROOT_SQL));
    if add_columns {
        plan.extend(
            [
                canonical::ADD_WORKSPACES_PROJECT_COLUMN_SQL,
                canonical::ADD_WORKSPACES_IDENTITY_COLUMN_SQL,
                canonical::ADD_WORKSPACES_DISCOVERY_COLUMN_SQL,
            ]
            .into_iter()
            .map(StatementSpec::bare),
        );
    }
    plan.extend(
        [
            canonical::BACKFILL_WORKSPACES_EVIDENCE_SQL,
            canonical::BACKFILL_MISSING_WORKSPACES_SQL,
        ]
        .into_iter()
        .map(StatementSpec::bare),
    );
    plan.extend(
        canonical::REBUILD_BINDINGS_WITHOUT_REGISTRATIONS_SQL
            .iter()
            .copied()
            .map(StatementSpec::bare),
    );
    plan.push(StatementSpec::bare(
        canonical::DROP_LEGACY_REGISTRATIONS_SQL,
    ));
    plan.push(StatementSpec::new(
        ADVANCE_VERSION_SQL,
        vec![
            Value::Integer(schema::REMOTE_SCHEMA_VERSION),
            Value::Text(schema::STORE_CONTRACT.to_owned()),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    ));
    Ok(plan)
}

fn column_names(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<String>> {
    rows.iter()
        .map(|row| Ok(text_at(row, 0).ok_or_else(unsupported)?.to_owned()))
        .collect()
}

#[cfg(test)]
#[path = "schema_v19_upgrade_test.rs"]
mod tests;
