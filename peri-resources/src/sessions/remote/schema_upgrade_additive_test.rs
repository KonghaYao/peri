//! 上一代当前形状 → 当前代数的增量批次与真实升级：只补派生对象，不碰会话事实。

use super::mutation::StoreAccess;
use super::schema::{self, StoreShapeRead, StoreSnapshot};
use super::schema_upgrade;
use super::schema_upgrade_tests::Fixture;

/// `v6` + 12 / `v5` + 11 的增量批次：守卫 → 派生索引表 → `thread_mq` → canonical 索引集
/// → 当前 Machine → 版本推进。
#[test]
fn additive_upgrade_plan_only_adds_derived_objects_and_advances_the_version() {
    use crate::sessions::canonical::{
        CREATE_INDEXES, CREATE_INHERITED_INDEX_TABLES, CREATE_THREAD_MQ_TABLE_SQL,
    };

    for (version, contract) in [
        (12, schema::PREVIOUS_CURRENT_STORE_CONTRACT),
        (11, schema::LEGACY_CURRENT_STORE_CONTRACT),
    ] {
        let snapshot = StoreSnapshot {
            store_id: schema::StoreId::mint(),
            schema_version: version,
            contract: contract.to_owned(),
        };
        let plan = schema_upgrade::additive_upgrade_plan(&snapshot, "test-machine").unwrap();
        let sql: Vec<&str> = plan.iter().map(|statement| statement.sql).collect();
        assert_eq!(sql[0], schema_upgrade::GUARD_META_SQL);
        let tables = 1..1 + CREATE_INHERITED_INDEX_TABLES.len();
        assert_eq!(&sql[tables.clone()], CREATE_INHERITED_INDEX_TABLES);
        assert_eq!(sql[tables.end], CREATE_THREAD_MQ_TABLE_SQL);
        let indexes = tables.end + 1..tables.end + 1 + CREATE_INDEXES.len();
        assert_eq!(&sql[indexes.clone()], CREATE_INDEXES);
        assert_eq!(
            sql[indexes.end],
            crate::sessions::canonical::INSERT_CURRENT_MACHINE_SQL
        );
        assert!(sql
            .last()
            .unwrap()
            .starts_with("UPDATE peri_store_meta SET schema_version"));
    }

    for (version, contract) in [
        (10, schema::PREVIOUS_CURRENT_STORE_CONTRACT),
        (11, schema::PREVIOUS_CURRENT_STORE_CONTRACT),
        (12, schema::LEGACY_CURRENT_STORE_CONTRACT),
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

/// `v6` + 12 的库升级到当前代：补 `thread_mq` 与索引、推进版本，不碰会话事实。
#[tokio::test]
async fn additive_upgrade_completes_a_previous_current_store() {
    let fixture = Fixture::previous_current().await;
    assert_eq!(fixture.version().await, 12);
    assert_eq!(
        fixture.contract().await,
        schema::PREVIOUS_CURRENT_STORE_CONTRACT
    );
    assert!(
        matches!(
            fixture
                .shape_verdict(schema::ShapeProbe::PreviousCurrent)
                .await,
            StoreShapeRead::Consistent
        ),
        "升级来源必须是健康的 v12 形状"
    );
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    assert_mq_present(&fixture, 3).await;
    assert_eq!(
        fixture
            .count("SELECT COUNT(*) FROM projects".to_owned())
            .await,
        1,
        "增量升级不得改写会话事实"
    );
    assert!(
        matches!(
            fixture.shape_verdict(schema::ShapeProbe::Current).await,
            StoreShapeRead::Consistent
        ),
        "升级终点必须与当前形状逐项相符"
    );
}

/// `v5` + 11 的库走同一条幂等批次：派生索引表与 `thread_mq` 一起补齐。
#[tokio::test]
async fn additive_upgrade_completes_a_legacy_current_store() {
    let fixture = Fixture::legacy_current().await;
    assert_eq!(fixture.version().await, 11);
    assert_eq!(
        fixture.contract().await,
        schema::LEGACY_CURRENT_STORE_CONTRACT
    );
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    assert_mq_present(&fixture, 3).await;
    assert!(matches!(
        fixture.shape_verdict(schema::ShapeProbe::Current).await,
        StoreShapeRead::Consistent
    ));
}

/// 升级终点同时有 `thread_mq` 与它的排序索引。
async fn assert_mq_present(fixture: &Fixture, expected_tables: i64) {
    let tables: i64 = fixture
        .count(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table'
             AND name IN ('inherited_snapshot_index', 'inherited_payload_index', 'thread_mq')"
                .to_owned(),
        )
        .await;
    assert_eq!(tables, expected_tables);
    let indexes: i64 = fixture
        .count(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index'
             AND name = 'idx_thread_mq_order'"
                .to_owned(),
        )
        .await;
    assert_eq!(indexes, 1);
}
