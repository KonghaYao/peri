//! `v5` + 11 → 当前形状的增量批次与真实升级：只补派生对象，不碰会话事实。

use super::mutation::StoreAccess;
use super::schema::{self, StoreShapeRead, StoreSnapshot};
use super::schema_upgrade;
use super::schema_upgrade_tests::Fixture;

/// `v5` + 11 的增量批次：守卫 → 派生索引表 → canonical 索引集 → 当前 Machine → 版本推进。
#[test]
fn additive_upgrade_plan_only_adds_derived_objects_and_advances_the_version() {
    use crate::sessions::canonical::{CREATE_INDEXES, CREATE_INHERITED_INDEX_TABLES};

    let snapshot = StoreSnapshot {
        store_id: schema::StoreId::mint(),
        schema_version: 11,
        contract: schema::PREVIOUS_CURRENT_STORE_CONTRACT.to_owned(),
    };
    let plan = schema_upgrade::additive_upgrade_plan(&snapshot, "test-machine").unwrap();
    let sql: Vec<&str> = plan.iter().map(|statement| statement.sql).collect();
    assert_eq!(sql[0], schema_upgrade::GUARD_META_SQL);
    let tables = 1..1 + CREATE_INHERITED_INDEX_TABLES.len();
    assert_eq!(&sql[tables.clone()], CREATE_INHERITED_INDEX_TABLES);
    let indexes = tables.end..tables.end + CREATE_INDEXES.len();
    assert_eq!(&sql[indexes.clone()], CREATE_INDEXES);
    assert_eq!(
        sql[indexes.end],
        crate::sessions::canonical::INSERT_CURRENT_MACHINE_SQL
    );
    assert!(sql
        .last()
        .unwrap()
        .starts_with("UPDATE peri_store_meta SET schema_version"));

    for (version, contract) in [
        (10, schema::PREVIOUS_CURRENT_STORE_CONTRACT),
        (11, schema::PREVIOUS_STORE_CONTRACT),
        (
            schema::REMOTE_SCHEMA_VERSION,
            schema::PREVIOUS_CURRENT_STORE_CONTRACT,
        ),
    ] {
        let snapshot = StoreSnapshot {
            store_id: schema::StoreId::mint(),
            schema_version: version,
            contract: contract.to_owned(),
        };
        assert!(
            schema_upgrade::additive_upgrade_plan(&snapshot, "test-machine").is_err(),
            "({version}, {contract}) 不是增量升级的输入"
        );
    }
}

/// 增量升级真的能落到一个 `v5` 形状的库上：补两表与索引、推进版本，不碰会话事实。
#[tokio::test]
async fn additive_upgrade_completes_a_previous_current_store() {
    let fixture = Fixture::previous_current().await;
    assert_eq!(fixture.version().await, 11);
    assert_eq!(
        fixture.contract().await,
        schema::PREVIOUS_CURRENT_STORE_CONTRACT
    );
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    let derived: i64 = fixture
        .count(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table'
             AND name IN ('inherited_snapshot_index', 'inherited_payload_index')"
                .to_owned(),
        )
        .await;
    assert_eq!(derived, 2);
    let projects: i64 = fixture
        .count("SELECT COUNT(*) FROM projects".to_owned())
        .await;
    assert_eq!(projects, 1, "增量升级不得改写会话事实");
    assert!(
        matches!(
            fixture.shape_verdict(schema::ShapeProbe::Current).await,
            StoreShapeRead::Consistent
        ),
        "升级终点必须与当前形状逐项相符"
    );
}
