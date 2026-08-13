//! Factory 测试（§5.6/§8.4.1）：schema_version 判空幂等补结构。

use yrs::types::ToJson;
use yrs::{Map, ReadTxn, Transact, WriteTxn};

use crate::state::factory::{DocKind, Factory, FactoryError, ROOT};

/// 读根 Map（不存在则 panic——结构断言的前提）。
fn root(txn: &yrs::Transaction) -> yrs::MapRef {
    txn.get_map(ROOT).expect("root map 存在")
}

#[test]
fn create_chat_doc_has_full_structure() {
    let f = Factory::new();
    let pair = f.create_chat_doc();
    let txn = pair.chat.transact();
    let root_map = root(&txn);
    let _ = &root_map;
    assert_eq!(
        root_map.get(&txn, "schema_version"),
        Some(yrs::Out::Any(1u32.into()))
    );
    assert_eq!(
        root_map.get(&txn, "projection_version"),
        Some(yrs::Out::Any(0u32.into()))
    );
    assert!(root_map
        .get(&txn, "entries")
        .unwrap()
        .cast::<yrs::MapRef>()
        .is_ok());
    assert!(root_map
        .get(&txn, "tool_calls")
        .unwrap()
        .cast::<yrs::MapRef>()
        .is_ok());
    assert!(root_map
        .get(&txn, "entry_order")
        .unwrap()
        .cast::<yrs::ArrayRef>()
        .is_ok());
    drop(txn);

    let txn = pair.session.transact();
    let root_map = root(&txn);
    let _ = &root_map;
    assert_eq!(
        root_map.get(&txn, "schema_version"),
        Some(yrs::Out::Any(1u32.into()))
    );
    assert!(root_map
        .get(&txn, "session")
        .unwrap()
        .cast::<yrs::MapRef>()
        .is_ok());
    assert!(root_map
        .get(&txn, "agent")
        .unwrap()
        .cast::<yrs::MapRef>()
        .is_ok());
    assert!(root_map
        .get(&txn, "pending_permissions")
        .unwrap()
        .cast::<yrs::MapRef>()
        .is_ok());
    assert!(root_map
        .get(&txn, "sessions")
        .unwrap()
        .cast::<yrs::MapRef>()
        .is_ok());
}

#[test]
fn ensure_schema_is_idempotent_on_fresh_doc() {
    let f = Factory::new();
    let mut doc = yrs::Doc::new();
    f.ensure_schema(&mut doc, DocKind::Chat).unwrap();
    let before = {
        let txn = doc.transact();
        root(&txn).to_json(&txn)
    };
    // 重复补结构：无重复、无覆盖（Any 深度相等，顺序无关）。
    f.ensure_schema(&mut doc, DocKind::Chat).unwrap();
    let after = {
        let txn = doc.transact();
        root(&txn).to_json(&txn)
    };
    assert_eq!(before, after);
}

#[test]
fn ensure_schema_patches_missing_keys_without_overwrite() {
    let f = Factory::new();
    let mut doc = yrs::Doc::new();
    {
        // 手工构造「缺键旧快照」：有 schema_version 但缺 entry_order。
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map(ROOT);
        root.insert(&mut txn, "schema_version", 1u32);
        root.insert(&mut txn, "projection_version", 5u32);
    }
    f.ensure_schema(&mut doc, DocKind::Chat).unwrap();
    let txn = doc.transact();
    let root_map = txn.get_map(ROOT).unwrap();
    // 已有数据不覆盖。
    assert_eq!(
        root_map.get(&txn, "projection_version"),
        Some(yrs::Out::Any(5u32.into()))
    );
    // 缺失键补齐。
    assert!(root_map.get(&txn, "entry_order").is_some());
    assert!(root_map.get(&txn, "entries").is_some());
    assert!(root_map.get(&txn, "tool_calls").is_some());
}

#[test]
fn future_schema_version_rejected() {
    let f = Factory::new();
    let mut doc = yrs::Doc::new();
    {
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map(ROOT);
        root.insert(&mut txn, "schema_version", 99u32);
    }
    let err = f.ensure_schema(&mut doc, DocKind::Chat).unwrap_err();
    assert!(matches!(
        err,
        FactoryError::FutureSchema {
            found: 99,
            expected: 1
        }
    ));
}

#[test]
fn registry_doc_has_global_status() {
    let f = Factory::new();
    let doc = f.create_registry_doc();
    let txn = doc.transact();
    let root_map = txn.get_map(ROOT).unwrap();
    assert_eq!(
        root_map.get(&txn, "schema_version"),
        Some(yrs::Out::Any(2u32.into()))
    );
    assert!(root_map
        .get(&txn, "instances")
        .unwrap()
        .cast::<yrs::MapRef>()
        .is_ok());
    assert!(root_map
        .get(&txn, "chats")
        .unwrap()
        .cast::<yrs::MapRef>()
        .is_ok());
    let global = root_map
        .get(&txn, "global")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(
        global.get(&txn, "status"),
        Some(yrs::Out::Any("healthy".into()))
    );
}

#[test]
fn session_doc_future_schema_rejected() {
    let f = Factory::new();
    let mut doc = yrs::Doc::new();
    {
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map(ROOT);
        root.insert(&mut txn, "schema_version", 7u32);
    }
    let err = f.ensure_schema(&mut doc, DocKind::Session).unwrap_err();
    assert!(matches!(err, FactoryError::FutureSchema { found: 7, .. }));
}
