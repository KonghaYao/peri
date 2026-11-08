//! 上一代当前形状 → 当前代数的增量批次与真实升级：补 `thread_mq` 与两列工作快照，
//! 再跑 v15 清理（三表重建、退役两表），不搬运会话事实。

use super::mutation::StoreAccess;
use super::schema::{self, StoreShapeRead, StoreSnapshot};
use super::schema_upgrade;
use super::schema_upgrade_tests::Fixture;

/// `v7` + 13 / `v6` + 12 / `v5` + 11 的增量批次：守卫 → `thread_mq` → 两列工作快照 →
/// 当前 Machine → v15 清理 → 版本推进。
#[test]
fn additive_upgrade_plan_only_extends_the_shape_and_advances_the_version() {
    use crate::sessions::canonical::{
        CREATE_INDEXES, CREATE_THREAD_MQ_TABLE_SQL, THREADS_V14_COLUMNS,
    };

    for (version, contract) in [
        (13, schema::LEGACY_CURRENT_STORE_CONTRACT),
        (12, schema::OLDER_CURRENT_STORE_CONTRACT),
        (11, schema::OLDEST_CURRENT_STORE_CONTRACT),
    ] {
        let snapshot = StoreSnapshot {
            store_id: schema::StoreId::mint(),
            schema_version: version,
            contract: contract.to_owned(),
        };
        let plan =
            schema_upgrade::additive_upgrade_plan(&snapshot, "test-machine", &[], &[]).unwrap();
        let sql: Vec<&str> = plan.iter().map(|statement| statement.sql).collect();
        assert_eq!(sql[0], schema_upgrade::GUARD_META_SQL);
        assert_eq!(sql[1], CREATE_THREAD_MQ_TABLE_SQL);
        for (statement, (name, _)) in sql[2..].iter().zip(THREADS_V14_COLUMNS) {
            assert!(
                statement.starts_with(&format!("ALTER TABLE threads ADD COLUMN {name} ")),
                "{statement} 必须是 {name} 的增量列"
            );
        }
        // 索引重放出现在版本推进之前、整份 canonical 清单（`INSERT_CURRENT_MACHINE_SQL`
        // 落在两者之间，因此按子序列核对）。
        let advance = sql.len() - 1;
        let replay: Vec<&str> = sql[..advance]
            .iter()
            .copied()
            .filter(|statement| CREATE_INDEXES.contains(statement))
            .collect();
        assert_eq!(replay, CREATE_INDEXES);
        assert!(sql[..advance].contains(&crate::sessions::canonical::INSERT_CURRENT_MACHINE_SQL));
        assert!(
            !sql.iter().any(|statement| statement
                .contains("CREATE TABLE IF NOT EXISTS inherited_snapshot_index")
                || statement.contains("CREATE TABLE IF NOT EXISTS inherited_payload_index")),
            "v15 起不得再建 inherited 派生索引表"
        );
        assert!(sql
            .last()
            .unwrap()
            .starts_with("UPDATE peri_store_meta SET schema_version"));
    }

    for (version, contract) in [
        (10, schema::PREVIOUS_CURRENT_STORE_CONTRACT),
        (11, schema::PREVIOUS_CURRENT_STORE_CONTRACT),
        (12, schema::PREVIOUS_CURRENT_STORE_CONTRACT),
        (13, schema::PREVIOUS_CURRENT_STORE_CONTRACT),
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
            schema_upgrade::additive_upgrade_plan(&snapshot, "test-machine", &[], &[]).is_err(),
            "({version}, {contract}) 不是增量升级的输入"
        );
    }
}

/// `v7` + 13 的库升级到当前代：补两列与索引、删两表、推进版本，不碰会话事实。
#[tokio::test]
async fn additive_upgrade_completes_a_previous_current_store() {
    let fixture = Fixture::previous_current().await;
    assert_eq!(fixture.version().await, 13);
    assert_eq!(
        fixture.contract().await,
        schema::LEGACY_CURRENT_STORE_CONTRACT
    );
    assert!(
        matches!(
            fixture
                .shape_verdict(schema::ShapeProbe::PreviousCurrent)
                .await,
            StoreShapeRead::Consistent
        ),
        "升级来源必须是健康的 v13 形状"
    );
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    // 增量批次补完 v14 的增量后接 v15 清理：终点就是当前形状。
    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    assert_mq_present(&fixture).await;
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

/// `v6` + 12 的库走同一条批次：`thread_mq` 与两列一起补齐。
#[tokio::test]
async fn additive_upgrade_completes_a_legacy_current_store() {
    let fixture = Fixture::legacy_current().await;
    assert_eq!(fixture.version().await, 12);
    assert_eq!(
        fixture.contract().await,
        schema::OLDER_CURRENT_STORE_CONTRACT
    );
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    assert_mq_present(&fixture).await;
    assert!(matches!(
        fixture.shape_verdict(schema::ShapeProbe::Current).await,
        StoreShapeRead::Consistent
    ));
}

/// `v5` + 11 的库也走同一条批次：`thread_mq` 与两列一起补齐。
#[tokio::test]
async fn additive_upgrade_completes_an_older_current_store() {
    let fixture = Fixture::older_current().await;
    assert_eq!(fixture.version().await, 11);
    assert_eq!(
        fixture.contract().await,
        schema::OLDEST_CURRENT_STORE_CONTRACT
    );
    schema_upgrade::upgrade(&fixture.store(StoreAccess::ReadWrite), &fixture.snapshot)
        .await
        .unwrap();

    assert_eq!(fixture.version().await, schema::REMOTE_SCHEMA_VERSION);
    assert_eq!(fixture.contract().await, schema::STORE_CONTRACT);
    assert_mq_present(&fixture).await;
    assert!(matches!(
        fixture.shape_verdict(schema::ShapeProbe::Current).await,
        StoreShapeRead::Consistent
    ));
}

/// 升级终点有 `thread_mq` 与它的排序索引。
async fn assert_mq_present(fixture: &Fixture) {
    let mq: i64 = fixture
        .count(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'thread_mq'"
                .to_owned(),
        )
        .await;
    assert_eq!(mq, 1);
    let indexes: i64 = fixture
        .count(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index'
             AND name = 'idx_thread_mq_order'"
                .to_owned(),
        )
        .await;
    assert_eq!(indexes, 1);
}
