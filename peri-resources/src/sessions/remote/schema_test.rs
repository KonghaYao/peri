//! `schema` 的离线测试：只读性、版本判定、参数化与形状拒绝。全部不联网。

use turso_serverless::Value;

use super::schema::{
    acceptance, decode_identity, identity_read_plan, initialization_plan, SchemaAcceptance,
    StoreId, StoreSnapshot, META_INSERT_INDEX, REMOTE_SCHEMA_VERSION, STORE_CONTRACT,
    STORE_META_TABLE,
};

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

#[test]
fn identity_read_plan_is_read_only() {
    let plan = identity_read_plan();
    assert_eq!(plan.len(), 2);
    for spec in &plan {
        assert!(
            spec.is_read_only(),
            "身份读取计划里出现非只读语句: {:?}",
            spec
        );
    }
    // 第一条只问表存在性（参数化），第二条才读元数据行。
    assert!(plan[0].sql.contains("sqlite_master"));
    assert!(plan[0].params.contains(&text(STORE_META_TABLE)));
    assert!(plan[1].sql.contains(STORE_META_TABLE));
}

#[test]
fn acceptance_rejects_unknown_versions() {
    assert_eq!(acceptance(REMOTE_SCHEMA_VERSION), SchemaAcceptance::Accept);
    assert_eq!(
        acceptance(REMOTE_SCHEMA_VERSION + 1),
        SchemaAcceptance::TooNew
    );
    assert_eq!(acceptance(0), SchemaAcceptance::Unusable);
    assert_eq!(acceptance(-1), SchemaAcceptance::Unusable);
}

#[test]
fn snapshot_must_match_contract_and_version() {
    let build = StoreSnapshot {
        store_id: StoreId::mint(),
        schema_version: REMOTE_SCHEMA_VERSION,
        contract: STORE_CONTRACT.to_owned(),
    };
    assert!(build.matches_build());

    // 统一之前的形状代数（`peri_sessions` 那套）：契约不认识就拒绝，不尝试迁移。
    let pre_unification = StoreSnapshot {
        contract: "peri.session.store/v1".to_owned(),
        ..build.clone()
    };
    assert!(!pre_unification.matches_build());

    let newer = StoreSnapshot {
        schema_version: REMOTE_SCHEMA_VERSION + 1,
        ..build.clone()
    };
    assert!(!newer.matches_build());
}

#[test]
fn initialization_plan_parameterizes_identity_and_never_overwrites() {
    let store_id = StoreId::mint();
    let plan = initialization_plan(&store_id, "2026-09-26T00:00:00+00:00");
    assert_eq!(plan.len(), 4);

    // 建表只用 IF NOT EXISTS：已存在即 no-op，绝不改写既有形状。
    for spec in &plan[..2] {
        assert!(
            spec.sql.contains("CREATE TABLE IF NOT EXISTS"),
            "{:?}",
            spec
        );
        assert!(spec.params.is_empty());
    }
    // 元数据行是 INSERT（主键竞争），不是 UPSERT/UPDATE；竞争下标与计划顺序绑定。
    assert_eq!(META_INSERT_INDEX, 2);
    assert!(plan[META_INSERT_INDEX]
        .sql
        .starts_with("INSERT INTO peri_store_meta"));
    assert!(plan[2].params.contains(&text(store_id.as_str())));
    assert!(
        !plan[2].sql.contains(store_id.as_str()),
        "store id 只能作为绑定参数出现"
    );
    assert!(
        !plan[2].sql.contains("2026-09-26"),
        "时间戳只能作为绑定参数出现"
    );
    // 最后一步读回本次写入的事实。
    assert!(plan[3].is_read_only());
}

#[test]
fn minted_store_ids_are_opaque_hex_and_unique() {
    let first = StoreId::mint();
    let second = StoreId::mint();
    assert_ne!(first, second);
    assert_eq!(first.as_str().len(), 32);
    assert!(first.as_str().chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn identity_decoding_rejects_shapes_it_cannot_explain() {
    let ok = decode_identity(&[
        Value::Integer(REMOTE_SCHEMA_VERSION),
        text("store-abc"),
        text(STORE_CONTRACT),
    ])
    .expect("well-formed row");
    assert_eq!(ok.store_id.as_str(), "store-abc");
    assert!(ok.matches_build());

    // 列缺失、类型不符、空行：一律拒绝，不猜。
    assert!(decode_identity(&[Value::Integer(1), text("s")]).is_none());
    assert!(decode_identity(&[Value::Real(1.0), text("s"), text(STORE_CONTRACT)]).is_none());
    assert!(decode_identity(&[Value::Null, Value::Null, Value::Null]).is_none());
    assert!(decode_identity(&[]).is_none());
}
