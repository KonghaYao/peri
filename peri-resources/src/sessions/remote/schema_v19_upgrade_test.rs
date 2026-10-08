//! `schema_v19_upgrade` 的离线测试：批次顺序、拒绝形状与守卫规则文本。全部不联网。

use turso_serverless::Value;

use super::*;
use crate::sessions::schema_cleanup::SchemaObject;

fn snapshot(version: i64, contract: &str) -> StoreSnapshot {
    StoreSnapshot {
        store_id: schema::StoreId::mint(),
        schema_version: version,
        contract: contract.to_owned(),
    }
}

fn v18() -> StoreSnapshot {
    snapshot(18, schema::PREVIOUS_STORE_CONTRACT)
}

fn registrations() -> Vec<String> {
    REGISTRATION_COLUMNS
        .iter()
        .map(|name| name.to_string())
        .collect()
}

fn workspaces() -> Vec<String> {
    vec![
        "id".into(),
        "machine_id".into(),
        "path".into(),
        "path_source".into(),
    ]
}

/// 由 v12 迁移用当前 `canonical` 常量建出的归属行：证据列已经在表上。
fn workspaces_with_evidence() -> Vec<String> {
    let mut columns = workspaces();
    columns.extend(ADDED_WORKSPACE_COLUMNS.iter().map(|name| name.to_string()));
    columns
}

fn plan_of(plan: &[StatementSpec]) -> Vec<&str> {
    plan.iter().map(|spec| spec.sql).collect()
}

#[test]
fn plan_refuses_versions_and_contracts_outside_the_previous_generation() {
    for wrong in [
        snapshot(18, "peri.session.store/v2"),
        snapshot(17, schema::PREVIOUS_STORE_CONTRACT),
        snapshot(19, schema::STORE_CONTRACT),
    ] {
        assert!(upgrade_plan(&wrong, &[], &registrations(), &workspaces()).is_err());
    }
}

#[test]
fn plan_refuses_unknown_registration_and_partially_migrated_workspace_shapes() {
    let incomplete: Vec<String> = registrations()
        .into_iter()
        .filter(|column| column != "discovery")
        .collect();
    assert!(upgrade_plan(&v18(), &[], &incomplete, &workspaces()).is_err());
    let mut partial = workspaces();
    partial.push("identity".into());
    assert!(upgrade_plan(&v18(), &[], &registrations(), &partial).is_err());
}

#[test]
fn plan_skips_column_adds_when_the_workspace_shape_already_carries_evidence() {
    let pre_migrated =
        upgrade_plan(&v18(), &[], &registrations(), &workspaces_with_evidence()).unwrap();
    let sql = plan_of(&pre_migrated);
    assert!(!sql.contains(&canonical::ADD_WORKSPACES_PROJECT_COLUMN_SQL));
    assert!(!sql.contains(&canonical::ADD_WORKSPACES_IDENTITY_COLUMN_SQL));
    assert!(!sql.contains(&canonical::ADD_WORKSPACES_DISCOVERY_COLUMN_SQL));
    assert!(sql.contains(&canonical::BACKFILL_WORKSPACES_EVIDENCE_SQL));
    assert!(sql.contains(&canonical::DROP_LEGACY_REGISTRATIONS_SQL));
    assert_eq!(sql.len(), 3 + 2 + 6 + 1 + 1);
}

#[test]
fn plan_orders_guards_before_writes_and_advances_version_last() {
    let snapshot = v18();
    let plan = upgrade_plan(
        &snapshot,
        &[SchemaObject {
            kind: "table".into(),
            name: "threads".into(),
            table: "threads".into(),
            sql: Some("CREATE TABLE threads (id TEXT)".into()),
        }],
        &registrations(),
        &workspaces(),
    )
    .unwrap();
    let sql = plan_of(&plan);
    assert_eq!(sql[0], super::super::schema_upgrade::GUARD_SNAPSHOT_SQL);
    assert_eq!(sql[1], super::super::schema_upgrade::GUARD_OBJECT_SQL);
    assert_eq!(sql[2], GUARD_BINDINGS_WITHOUT_OWNER_SQL);
    assert_eq!(sql[3], GUARD_BINDINGS_AT_FOREIGN_ROOT_SQL);
    let index = |needle: &str| {
        sql.iter()
            .position(|statement| *statement == needle)
            .unwrap_or_else(|| panic!("missing statement: {needle}"))
    };
    let adds = index(canonical::ADD_WORKSPACES_PROJECT_COLUMN_SQL);
    assert_eq!(
        adds + 1,
        index(canonical::ADD_WORKSPACES_IDENTITY_COLUMN_SQL)
    );
    assert_eq!(
        adds + 2,
        index(canonical::ADD_WORKSPACES_DISCOVERY_COLUMN_SQL)
    );
    let evidence = index(canonical::BACKFILL_WORKSPACES_EVIDENCE_SQL);
    assert_eq!(
        evidence + 1,
        index(canonical::BACKFILL_MISSING_WORKSPACES_SQL)
    );
    let rebuild = index(canonical::REBUILD_BINDINGS_WITHOUT_REGISTRATIONS_SQL[0]);
    assert_eq!(
        index(canonical::REBUILD_BINDINGS_WITHOUT_REGISTRATIONS_SQL[3]),
        rebuild + 3
    );
    assert_eq!(index(canonical::DROP_LEGACY_REGISTRATIONS_SQL), rebuild + 6);
    assert_eq!(sql.len() - 1, index(ADVANCE_VERSION_SQL));
    assert_eq!(sql.len(), 4 + 5 + 6 + 1 + 1);
    // 版本推进绑定的是本构建的版本与契约，源版本与源契约写死在语句里：只有它们同时成立
    // 才改元数据行，改不动时批次结果计数不是 1，升级按失败上报。
    assert_eq!(
        plan.last().unwrap().params,
        vec![
            Value::Integer(schema::REMOTE_SCHEMA_VERSION),
            Value::Text(schema::STORE_CONTRACT.to_owned()),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(schema::PREVIOUS_STORE_CONTRACT.to_owned()),
        ]
    );
}

#[test]
fn guard_text_carries_the_same_rule_as_the_local_migration() {
    // 远端没有事务内读回，同一条规则只能换载体；文本必须包住 `canonical` 那一份，
    // 否则两端的「哪些绑定不能搬运」会各自漂移。
    assert_eq!(
        GUARD_BINDINGS_WITHOUT_OWNER_SQL,
        format!(
            "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE ({}) <> 0",
            canonical::COUNT_BINDINGS_WITHOUT_OWNER_SQL
        )
    );
    assert_eq!(
        GUARD_BINDINGS_AT_FOREIGN_ROOT_SQL,
        format!(
            "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE ({}) <> 0",
            canonical::COUNT_BINDINGS_AT_FOREIGN_ROOT_SQL
        )
    );
}
