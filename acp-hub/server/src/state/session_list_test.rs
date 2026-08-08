//! SessionList 全量同步投影测试（§6.3/§5.2）：diff 纯函数 + apply_diff 原语。

use std::collections::HashMap;

use yrs::{Map, ReadTxn, Transact, WriteTxn};

use acp_hub_proto::schema::SessionSummaryProjection;

use crate::state::factory::{Factory, ROOT};
use crate::state::session_list::{apply_diff, diff, SessionListDiff};

fn sum(id: &str, title: &str, updated: &str) -> SessionSummaryProjection {
    SessionSummaryProjection {
        session_id: id.to_string(),
        title: title.to_string(),
        status: "completed".to_string(),
        updated_at: updated.to_string(),
    }
}

fn map_of(entries: &[SessionSummaryProjection]) -> HashMap<String, SessionSummaryProjection> {
    entries
        .iter()
        .map(|e| (e.session_id.clone(), e.clone()))
        .collect()
}

#[test]
fn diff_upserts_new_and_removes_stale() {
    let current = map_of(&[sum("s1", "a", "t0"), sum("s2", "b", "t0")]);
    let incoming = vec![sum("s1", "a", "t0"), sum("s3", "c", "t0")];
    let d = diff(&current, &incoming);
    assert_eq!(d.upsert, vec![sum("s3", "c", "t0")]);
    assert_eq!(d.remove, vec!["s2".to_string()]);
}

#[test]
fn diff_upserts_changed_fields() {
    let current = map_of(&[sum("s1", "a", "t0")]);
    let incoming = vec![sum("s1", "a2", "t1")];
    let d = diff(&current, &incoming);
    assert_eq!(d.upsert, vec![sum("s1", "a2", "t1")]);
    assert!(d.remove.is_empty());
}

#[test]
fn diff_no_change_is_noop() {
    let current = map_of(&[sum("s1", "a", "t0"), sum("s2", "b", "t0")]);
    let incoming = vec![sum("s1", "a", "t0"), sum("s2", "b", "t0")];
    let d = diff(&current, &incoming);
    assert_eq!(d, SessionListDiff { upsert: vec![], remove: vec![] });
}

#[test]
fn diff_empty_incoming_removes_all() {
    let current = map_of(&[sum("s1", "a", "t0")]);
    let d = diff(&current, &[]);
    assert_eq!(d.remove, vec!["s1".to_string()]);
}

#[test]
fn apply_diff_writes_and_removes() {
    let mut pair = Factory::new().create_chat_doc();
    {
        // 预置 s2。
        let mut txn = pair.session_txn();
        let root = txn.get_or_insert_map(ROOT);
        let d = SessionListDiff {
            upsert: vec![sum("s2", "b", "t0")],
            remove: vec![],
        };
        apply_diff(&mut txn, &root, &d);
    }
    let d = SessionListDiff {
        upsert: vec![sum("s1", "a", "t1")],
        remove: vec!["s2".to_string()],
    };
    {
        let mut txn = pair.session_txn();
        let root = txn.get_or_insert_map(ROOT);
        apply_diff(&mut txn, &root, &d);
    }
    // 断言 Map 与响应一致。
    let txn = pair.session.transact();
    let root = txn.get_map(ROOT).unwrap();
    let sessions = root.get(&txn, "sessions").unwrap().cast::<yrs::MapRef>().unwrap();
    assert_eq!(sessions.len(&txn), 1);
    let sm = sessions.get(&txn, "s1").unwrap().cast::<yrs::MapRef>().unwrap();
    assert_eq!(
        sm.get(&txn, "title"),
        Some(yrs::Out::Any("a".into()))
    );
    assert!(sessions.get(&txn, "s2").is_none());
}

#[test]
fn read_current_roundtrip() {
    let mut pair = Factory::new().create_chat_doc();
    {
        let mut txn = pair.session_txn();
        let root = txn.get_or_insert_map(ROOT);
        let d = SessionListDiff {
            upsert: vec![sum("s1", "a", "t1")],
            remove: vec![],
        };
        apply_diff(&mut txn, &root, &d);
    }
    let txn = pair.session.transact();
    let root = txn.get_map(ROOT).unwrap();
    let current = crate::state::session_list::read_current(&txn, &root);
    assert_eq!(current.len(), 1);
    assert_eq!(current.get("s1").unwrap().title, "a");
}
