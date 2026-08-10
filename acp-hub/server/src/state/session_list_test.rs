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
        cwd: String::new(),
        bound_chat_id: None,
    }
}

fn sum_cwd(id: &str, title: &str, updated: &str, cwd: &str) -> SessionSummaryProjection {
    SessionSummaryProjection {
        session_id: id.to_string(),
        title: title.to_string(),
        status: "completed".to_string(),
        updated_at: updated.to_string(),
        cwd: cwd.to_string(),
        bound_chat_id: None,
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
        let mut txn = pair.control_txn();
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
        let mut txn = pair.control_txn();
        let root = txn.get_or_insert_map(ROOT);
        apply_diff(&mut txn, &root, &d);
    }
    // 断言 Map 与响应一致。
    let txn = pair.control.transact();
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
        let mut txn = pair.control_txn();
        let root = txn.get_or_insert_map(ROOT);
        let d = SessionListDiff {
            upsert: vec![sum("s1", "a", "t1")],
            remove: vec![],
        };
        apply_diff(&mut txn, &root, &d);
    }
    let txn = pair.control.transact();
    let root = txn.get_map(ROOT).unwrap();
    let current = crate::state::session_list::read_current(&txn, &root);
    assert_eq!(current.len(), 1);
    assert_eq!(current.get("s1").unwrap().title, "a");
}

// ── per-cwd 全量同步（workspace 扩展，§6.3）─────────────────────────────

#[test]
fn diff_per_cwd_full_sync_isolated() {
    // current：s1(/ws-a 更新), s2(/ws-a 删除), s3(/ws-b 无轮询面保留)。
    let current = map_of(&[
        sum_cwd("s1", "a", "t0", "/ws-a"),
        sum_cwd("s2", "b", "t0", "/ws-a"),
        sum_cwd("s3", "c", "t0", "/ws-b"),
    ]);
    let incoming = vec![
        sum_cwd("s1", "a", "t1", "/ws-a"),
        sum_cwd("s4", "d", "t0", "/ws-a"),
    ];
    let d = diff(&current, &incoming);
    // 同 cwd 全量同步：s2 不在响应 → 删；s3 跨 cwd 且无轮询面 → 保留。
    assert_eq!(d.remove, vec!["s2".to_string()]);
    // upsert：s1 字段变化、s4 新增；s3 不变。
    assert!(d.upsert.iter().any(|e| e.session_id == "s1" && e.updated_at == "t1"));
    assert!(d.upsert.iter().any(|e| e.session_id == "s4"));
    assert!(!d.upsert.iter().any(|e| e.session_id == "s3"));
}

#[test]
fn diff_cwdless_legacy_removed_while_other_cwd_kept() {
    // 历史遗留条目（无 cwd 字段 → 空串）随任意轮询删除；有真实 cwd 的
    // 条目即使不在本轮响应面也保留。
    let current = map_of(&[sum("s1", "a", "t0"), sum_cwd("s2", "b", "t0", "/ws-a")]);
    let incoming = vec![sum_cwd("s2", "b", "t0", "/ws-a")];
    let d = diff(&current, &incoming);
    assert_eq!(d.remove, vec!["s1".to_string()]);
    assert!(d.upsert.is_empty(), "s2 无变化 no-op");
}

#[test]
fn diff_orphan_key_removed_even_when_internal_matches() {
    // 孤儿 key（map key ≠ 内部 session_id）：内部 session_id/cwd 恰好与
    // incoming 相同也须删除——per-cwd 匹配会误判「在响应中」，孤儿 key
    // 永存则渲染层重复条目（§6.3 自愈；正常条目由 upsert 侧重建）。
    let mut current = map_of(&[sum_cwd("s1", "a", "t0", "/ws-a")]);
    current.insert("old-key".to_string(), sum_cwd("s1", "a", "t0", "/ws-a"));
    let incoming = vec![sum_cwd("s1", "a", "t0", "/ws-a")];
    let d = diff(&current, &incoming);
    assert!(d.remove.contains(&"old-key".to_string()));
    assert!(!d.remove.contains(&"s1".to_string()));
    assert!(d.upsert.is_empty(), "s1 正常条目无变化");
}
