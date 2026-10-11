//! `schema` 的离线测试：只读性、版本判定、参数化、形状拒绝与打开结论。全部不联网。

use std::collections::HashMap;

use peri_acp_types::session_resources::SessionResourceErrorKind;
use turso_serverless::Value;

use super::mutation::StoreAccess;
use super::schema::{
    acceptance, decode_identity, identity_read_plan, initialization_plan, shape_probe,
    SchemaAcceptance, ShapeProbe, StoreId, StoreIdentityRead, StoreShapeRead, StoreSnapshot,
    META_INSERT_INDEX, PREVIOUS_STORE_CONTRACT, REMOTE_SCHEMA_VERSION, STORE_CONTRACT,
    STORE_META_TABLE,
};
use super::session_data::{open_step, OpenStep};
use super::sql::StatementSpec;
use crate::sessions::schema_shape::{self, ActualColumn};

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

/// 元数据里的一个身份：契约与版本是变量，store id 只是占位。
fn snapshot(version: i64, contract: &str) -> StoreSnapshot {
    StoreSnapshot {
        store_id: StoreId::mint(),
        schema_version: version,
        contract: contract.to_owned(),
    }
}

/// 声明式形状 → 逐表列形状（判定入口的输入形态），用于离线判定漂移。
fn declared_shape_map() -> HashMap<String, Vec<ActualColumn>> {
    let mut rows = Vec::new();
    for table in schema_shape::CURRENT_TABLES {
        for column in table.columns {
            rows.push((
                table.name.to_owned(),
                (*column).to_owned(),
                table
                    .not_null
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(column)),
                i64::from(
                    table
                        .primary_key
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(column)),
                ),
            ));
        }
    }
    schema_shape::group_columns(rows)
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
    assert_eq!(acceptance(10), SchemaAcceptance::Upgradeable);
    // 未发布的中间代（12..19）：都是「比本构建新」，不迁移也不降级写入。
    for version in 12..=19 {
        assert_eq!(
            acceptance(version),
            SchemaAcceptance::TooNew,
            "版本 {version} 是未发布的中间代"
        );
    }
    assert_eq!(acceptance(9), SchemaAcceptance::Unusable);
    assert_eq!(acceptance(0), SchemaAcceptance::Unusable);
    assert_eq!(acceptance(-1), SchemaAcceptance::Unusable);
}

/// 读取面只有两个代数：本代（`v5` + 11）与统一前的 `v2`（10|11）。其余一律不认识。
#[test]
fn only_the_compatible_contract_and_generation_are_readable() {
    assert!(snapshot(REMOTE_SCHEMA_VERSION, STORE_CONTRACT).readable());
    assert!(snapshot(10, PREVIOUS_STORE_CONTRACT).readable());
    assert!(snapshot(11, PREVIOUS_STORE_CONTRACT).readable());
    // 版本与契约必须互相匹配：`v5` 不是 10 的当前代数，`v2` 也不覆盖 12 之后的形状。
    assert!(!snapshot(10, STORE_CONTRACT).readable());
    assert!(!snapshot(REMOTE_SCHEMA_VERSION, PREVIOUS_STORE_CONTRACT).matches_build());
    for version in [9, 12, 13, 14, 15, 16, 17, 18, 19] {
        assert!(
            !snapshot(version, PREVIOUS_STORE_CONTRACT).readable(),
            "({version}, {PREVIOUS_STORE_CONTRACT})"
        );
    }
    for contract in [
        "peri.session.store/v1",
        "peri.session.store/v3",
        "peri.session.store/v4",
        "unknown-contract",
    ] {
        for version in [10, 11, 12, 19] {
            assert!(
                !snapshot(version, contract).readable(),
                "({version}, {contract})"
            );
        }
    }
}

/// 拒绝面：不认识 / 不可读的库在 `open_step` 上就被拦下——不进迁移，也不靠形状探测放行。
#[test]
fn open_step_refuses_unreadable_stores() {
    for (version, contract) in [
        (9, PREVIOUS_STORE_CONTRACT),
        (12, PREVIOUS_STORE_CONTRACT),
        (13, PREVIOUS_STORE_CONTRACT),
        (14, PREVIOUS_STORE_CONTRACT),
        (15, PREVIOUS_STORE_CONTRACT),
        (16, PREVIOUS_STORE_CONTRACT),
        (17, PREVIOUS_STORE_CONTRACT),
        (18, PREVIOUS_STORE_CONTRACT),
        (19, PREVIOUS_STORE_CONTRACT),
        (10, "peri.session.store/v3"),
        (11, "peri.session.store/v4"),
        (10, STORE_CONTRACT),
        (REMOTE_SCHEMA_VERSION, "unknown-contract"),
    ] {
        let read = StoreIdentityRead::Present(snapshot(version, contract));
        for access in [StoreAccess::ReadOnly, StoreAccess::ReadWrite] {
            let error = open_step(read.clone(), None, access).unwrap_err();
            assert!(
                matches!(error.kind(), SessionResourceErrorKind::Unsupported),
                "({version}, {contract}, {access:?}) 必须被拒绝"
            );
        }
    }
}

/// 形状探测与读取面同一条判据：读得动的库必须探得清，不读的库不探。
#[test]
fn shape_probe_follows_the_readable_surface() {
    assert_eq!(
        shape_probe(&snapshot(REMOTE_SCHEMA_VERSION, STORE_CONTRACT)),
        Some(ShapeProbe::Current)
    );
    for version in [10, 11] {
        assert_eq!(
            shape_probe(&snapshot(version, PREVIOUS_STORE_CONTRACT)),
            Some(ShapeProbe::Input),
            "版本 {version} 的 v2 库按迁移输入形状探测"
        );
    }
    for (version, contract) in [
        (10, STORE_CONTRACT),
        (9, PREVIOUS_STORE_CONTRACT),
        (12, PREVIOUS_STORE_CONTRACT),
        (19, PREVIOUS_STORE_CONTRACT),
        (11, "peri.session.store/v3"),
        (11, "peri.session.store/v4"),
    ] {
        assert_eq!(
            shape_probe(&snapshot(version, contract)),
            None,
            "({version}, {contract}) 不在读取面内，没有可探的形状"
        );
    }
}

/// 探测只发 SELECT：形状探测不能建表、不能写行，也不带参数。
#[test]
fn shape_probes_are_read_only_selects() {
    for probe in [ShapeProbe::Current, ShapeProbe::Input] {
        let spec = StatementSpec::bare(probe.sql());
        assert!(spec.is_read_only(), "{probe:?} 必须是只读语句");
        assert!(spec.params.is_empty(), "{probe:?} 不带参数");
        assert!(spec.sql.contains("pragma_table_xinfo"), "{probe:?}");
    }
}

/// 形状判定：相符才放行；不符要点出**哪张表哪一列**（诊断要能回答「为什么」）。
#[test]
fn shape_probe_reports_which_table_drifted() {
    let mut tables = declared_shape_map();
    assert_eq!(
        ShapeProbe::Current.check(&tables),
        StoreShapeRead::Consistent
    );

    let columns = tables.get_mut("threads").expect("threads 在声明里");
    columns.pop();
    let verdict = ShapeProbe::Current.check(&tables);
    assert!(
        matches!(&verdict, StoreShapeRead::Drifted(detail) if detail.contains("threads")),
        "{verdict:?}"
    );

    // 缺表就是漂移：本代读得动的前提是声明里的表都在，且逐列相符。
    let mut missing_bindings = declared_shape_map();
    missing_bindings.remove("session_bindings");
    let verdict = ShapeProbe::Current.check(&missing_bindings);
    assert!(
        matches!(&verdict, StoreShapeRead::Drifted(detail) if detail.contains("session_bindings")),
        "{verdict:?}"
    );

    // 一张都没建：不是漂移，而是「形状还没建立」（写打开补齐/升级，只读打开拒绝）。
    assert_eq!(
        ShapeProbe::Current.check(&HashMap::new()),
        StoreShapeRead::Unbuilt
    );
    assert_eq!(
        ShapeProbe::Input.check(&HashMap::new()),
        StoreShapeRead::Unbuilt
    );
}

/// 打开结论：形状相符才服务；形状漂移或没探过都不放行。
#[test]
fn open_step_requires_a_matching_shape_before_serving() {
    let build = snapshot(REMOTE_SCHEMA_VERSION, STORE_CONTRACT);
    let read = StoreIdentityRead::Present(build.clone());
    assert_eq!(
        open_step(
            read.clone(),
            Some(StoreShapeRead::Consistent),
            StoreAccess::ReadWrite
        )
        .unwrap(),
        OpenStep::Existing(build.store_id.clone())
    );

    // 漂移：拒绝（分类仍是「不认识的 store schema」），诊断在日志里。
    let drifted = StoreShapeRead::Drifted("table threads column sequence drifted".to_owned());
    for access in [StoreAccess::ReadOnly, StoreAccess::ReadWrite] {
        let error = open_step(read.clone(), Some(drifted.clone()), access).unwrap_err();
        assert!(matches!(
            error.kind(),
            SessionResourceErrorKind::Unsupported
        ));
    }
    // 读得动却没探过形状：内部矛盾，同样不放行。
    let error = open_step(read, None, StoreAccess::ReadWrite).unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Internal { .. }
    ));

    // 会话表还没建立：写打开照旧补齐（本代幂等 DDL / v2 的空库分支），只读打开拒绝。
    let unbuilt = StoreIdentityRead::Present(snapshot(REMOTE_SCHEMA_VERSION, STORE_CONTRACT));
    assert!(matches!(
        open_step(
            unbuilt.clone(),
            Some(StoreShapeRead::Unbuilt),
            StoreAccess::ReadWrite
        )
        .unwrap(),
        OpenStep::Existing(_)
    ));
    let error = open_step(
        unbuilt,
        Some(StoreShapeRead::Unbuilt),
        StoreAccess::ReadOnly,
    )
    .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Unsupported
    ));

    // 统一前的形状：只读服务、写打开先升级——两者都必须形状相符。
    let previous = StoreIdentityRead::Present(snapshot(10, PREVIOUS_STORE_CONTRACT));
    assert!(matches!(
        open_step(
            previous.clone(),
            Some(StoreShapeRead::Consistent),
            StoreAccess::ReadWrite
        )
        .unwrap(),
        OpenStep::Upgrade(_)
    ));
    assert!(matches!(
        open_step(
            previous.clone(),
            Some(StoreShapeRead::Consistent),
            StoreAccess::ReadOnly
        )
        .unwrap(),
        OpenStep::Existing(_)
    ));
    for access in [StoreAccess::ReadOnly, StoreAccess::ReadWrite] {
        let error = open_step(previous.clone(), Some(drifted.clone()), access).unwrap_err();
        assert!(
            matches!(error.kind(), SessionResourceErrorKind::Unsupported),
            "v2 库的形状漂移同样拒绝（{access:?}）"
        );
    }
    // v2 的空库：写打开走升级（空库分支直接建当前形状），只读打开没有可读的事实。
    assert!(matches!(
        open_step(
            previous.clone(),
            Some(StoreShapeRead::Unbuilt),
            StoreAccess::ReadWrite
        )
        .unwrap(),
        OpenStep::Upgrade(_)
    ));
    let error = open_step(
        previous,
        Some(StoreShapeRead::Unbuilt),
        StoreAccess::ReadOnly,
    )
    .unwrap_err();
    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::Unsupported
    ));
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
