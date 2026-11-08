//! `v8` + 14 → `v9` + 15 的远端清理批次：三表重建、两表退役、绑定证据投影。
//!
//! 这一代没有新列，只有**删除**：`projects.object_identity`、`workspaces.identity`、
//! `session_bindings.evidence_origin` 退役，唯一键收窄为 `UNIQUE(locator)`，绑定行的
//! 创建证据收缩为最小执行根。批次仍是一个受管事务：守卫在前，版本推进在最后。

use peri_acp_types::session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta};
use peri_acp_types::thread::{CancelPolicy, ThreadId};

use super::mutation::StoreAccess;
use super::schema::{self, StoreShapeRead};
use super::schema_upgrade;
use super::schema_upgrade_tests::{Fixture, DISCOVERY};
use super::session_sql;

const LEGACY_SNAPSHOT: &str = r#"{"root":"/work","root_identity":{"device":1,"inode":2},"common_dir":"/work/.git","common_identity":{"device":1,"inode":3},"private_dir":"/work/.git","private_identity":{"device":1,"inode":4}}"#;

async fn seed_v14_rows(fixture: &Fixture) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO machines(id, name, identity_kind) VALUES ('machine', '机器', 'known');
         INSERT INTO workspaces(id, machine_id, path, path_source, project_id, identity, discovery)
         VALUES ('11111111-1111-4111-8111-111111111111', 'machine', '/work', 'discovered',
                 'project', '{{\"device\":1,\"inode\":2}}', '{DISCOVERY}');
         INSERT INTO threads(id, cwd, created_at, updated_at, message_count, workspace_id)
         VALUES ('session', '/work', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', 1,
                 '11111111-1111-4111-8111-111111111111');
         INSERT INTO messages(message_id, thread_id, role, content)
         VALUES ('message', 'session', 'user', 'kept history');
         INSERT INTO session_bindings(thread_id, schema_version, project_id, workspace_id,
                                      relative_cwd, discovery_snapshot, evidence_origin)
         VALUES ('session', 1, 'project', '11111111-1111-4111-8111-111111111111', '',
                 '{LEGACY_SNAPSHOT}', 'creation_snapshot');"
    )))
    .execute(&fixture.transport.pool)
    .await
    .unwrap();
}

/// v14 的库被识别为可读可升级来源，且形状探测按 v14 冻结声明判成一致。
#[tokio::test]
async fn v14_store_is_readable_and_probes_as_the_previous_generation() {
    let fixture = Fixture::previous_generation().await;
    assert_eq!(fixture.version().await, 14);
    assert_eq!(
        fixture.contract().await,
        schema::PREVIOUS_CURRENT_STORE_CONTRACT
    );
    assert_eq!(
        schema::shape_probe(&fixture.snapshot),
        Some(schema::ShapeProbe::PreviousCurrent)
    );
    assert!(matches!(
        fixture
            .shape_verdict(schema::ShapeProbe::PreviousCurrent)
            .await,
        StoreShapeRead::Consistent
    ));
}

/// v14 → v15：一次受管批次删掉三列与两表、把证据收缩为最小执行根，并推进版本与契约。
#[tokio::test]
async fn v14_batch_cleans_and_advances() {
    let fixture = Fixture::previous_generation().await;
    seed_v14_rows(&fixture).await;
    let plan = {
        let store = fixture.store(StoreAccess::ReadWrite);
        let read = schema_upgrade::read_input(&store).await.unwrap();
        schema_upgrade::v15_cleanup_plan(&fixture.snapshot, &read, "test-machine").unwrap()
    };
    // 末条必须是版本推进（`apply_schema_upgrade` 按「末条影响 1 行」确认落地）。
    assert!(plan
        .last()
        .unwrap()
        .sql
        .starts_with("UPDATE peri_store_meta SET schema_version"));
    // 三张重建表各只出现一次；两张退役表只有 DROP。
    for sql in [
        crate::sessions::canonical::CREATE_REBUILD_PROJECTS_TABLE_SQL,
        crate::sessions::canonical::CREATE_REBUILD_WORKSPACES_TABLE_SQL,
        crate::sessions::canonical::CREATE_REBUILD_BINDINGS_TABLE_SQL,
    ] {
        assert_eq!(
            plan.iter().filter(|statement| statement.sql == sql).count(),
            1,
            "每张表只重建一次：{sql}"
        );
    }

    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    assert!(matches!(
        fixture.shape_verdict(schema::ShapeProbe::Current).await,
        StoreShapeRead::Consistent
    ));

    // 三列退役。
    for (table, column) in [
        ("projects", "object_identity"),
        ("workspaces", "identity"),
        ("session_bindings", "evidence_origin"),
    ] {
        assert_eq!(
            fixture
                .count(format!(
                    "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = '{column}'"
                ))
                .await,
            0,
            "{table}.{column} 必须退役"
        );
    }
    // 两张派生索引表退役，历史与会话行原样保留。
    assert_eq!(
        fixture
            .count(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table'
                 AND name IN ('inherited_snapshot_index', 'inherited_payload_index')"
                    .to_owned()
            )
            .await,
        0
    );
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM threads".to_owned())
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM messages".to_owned())
            .await,
        1
    );
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM workspaces".to_owned())
            .await,
        1
    );
    // 绑定证据收缩为最小执行根：native 只有 `root`。
    let (snapshot,): (String,) = sqlx::query_as(
        "SELECT discovery_snapshot FROM session_bindings WHERE thread_id = 'session'",
    )
    .fetch_one(&fixture.transport.pool)
    .await
    .unwrap();
    assert_eq!(snapshot, r#"{"root":"/work"}"#);
    // 归属行的完整发现快照不做清洗（它仍是诊断依据）。
    let (discovery,): (String,) = sqlx::query_as(
        "SELECT discovery FROM workspaces WHERE id = '11111111-1111-4111-8111-111111111111'",
    )
    .fetch_one(&fixture.transport.pool)
    .await
    .unwrap();
    assert_eq!(discovery, DISCOVERY);
}

/// 损坏的创建证据拒绝整批：不静默降级成 NULL（那会把可执行会话变成只读历史）。
#[tokio::test]
async fn v14_batch_refuses_a_corrupted_binding_snapshot() {
    let fixture = Fixture::previous_generation().await;
    seed_v14_rows(&fixture).await;
    sqlx::query(
        "UPDATE session_bindings SET discovery_snapshot = 'not json' WHERE thread_id = 'session'",
    )
    .execute(&fixture.transport.pool)
    .await
    .unwrap();

    let error = schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Unsupported
        ),
        "损坏证据必须按不支持拒绝: {error:?}"
    );
    // 整批回滚：版本与契约原样，行也还在。
    assert_eq!(fixture.version().await, 14);
    assert_eq!(
        fixture.contract().await,
        schema::PREVIOUS_CURRENT_STORE_CONTRACT
    );
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM session_bindings".to_owned())
            .await,
        1
    );
}

/// 写入侧只落最小执行根：即使调用方把完整发现快照塞进 `SessionInsert`，语句绑定值也被
/// 投影（native 只有 `root`），不会把身份与 Git 布局写进绑定行。
#[test]
fn binding_insert_projects_a_full_discovery_snapshot() {
    use peri_acp_types::workspace::{
        ProjectId, SessionBinding, WorkspaceId, SESSION_BINDING_VERSION,
    };

    let binding = SessionBinding {
        schema_version: SESSION_BINDING_VERSION,
        revision: 1,
        project_id: ProjectId::new(),
        workspace_id: WorkspaceId::new(),
        cwd_relative_to_workspace: std::path::PathBuf::new(),
    };
    let input = NewSession {
        thread_id: ThreadId::from("session".to_owned()),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            title: None,
            cwd: "/work".to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: None,
        },
        binding,
        frozen: FrozenSnapshotBytes::new("{}"),
    };
    let mut row = session_sql::session_insert(&input, 0, None);
    row.discovery_snapshot = Some(LEGACY_SNAPSHOT.to_owned());
    let statements = session_sql::insert_session_statements(&row).unwrap();
    let binding = statements
        .iter()
        .find(|statement| statement.sql.contains("INTO session_bindings"))
        .expect("绑定行必须写入");
    let bound = binding
        .params
        .iter()
        .find_map(|value| match value {
            turso_serverless::Value::Text(text) if text.starts_with('{') => Some(text.clone()),
            _ => None,
        })
        .expect("绑定语句必须带证据");
    assert_eq!(bound, r#"{"root":"/work"}"#);
}
