//! Permission CAS 测试（§7.4 规则 4 / §5.4）：pending → resolved 原子一次。

use yrs::{Map, Transact, WriteTxn};

use acp_hub_proto::action::PermissionDecision;

use crate::state::chat_writer;
use crate::state::doc_pair::DocPair;
use crate::state::factory::{Factory, ROOT};
use crate::state::permission::{expire, resolve, CasOutcome};

fn pair_with_permission(id: &str) -> DocPair {
    let mut p = Factory::new().create_chat_doc();
    // 直接写入 pending_permissions（模拟聚合器 PermissionRequested 投影后）。
    let mut txn = p.session_txn();
    let root = txn.get_or_insert_map(ROOT);
    let perms = root.get_or_init::<_, yrs::MapRef>(&mut txn, "pending_permissions");
    let pm = perms.get_or_init::<_, yrs::MapRef>(&mut txn, id);
    pm.insert(&mut txn, "permission_id", id.to_string());
    pm.insert(&mut txn, "turn_id", "t1".to_string());
    pm.insert(&mut txn, "status", "pending".to_string());
    pm.insert(&mut txn, "decision", yrs::Any::Null);
    pm.insert(&mut txn, "expires_at", "2026-08-07T00:05:00Z".to_string());
    drop(txn);
    p
}

fn read_status_and_decision(p: &DocPair, id: &str) -> (String, Option<yrs::Out>) {
    let txn = p.session.transact();
    let root = chat_writer::root_map_read(&txn).unwrap();
    let perms = root
        .get(&txn, "pending_permissions")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    let pm = perms.get(&txn, id).unwrap().cast::<yrs::MapRef>().unwrap();
    let status = pm
        .get(&txn, "status")
        .and_then(|s| s.cast::<String>().ok())
        .unwrap_or_default();
    let decision = pm.get(&txn, "decision");
    (status, decision)
}

#[test]
fn resolve_pending_migrates_once_with_decision() {
    let mut p = pair_with_permission("p1");
    assert_eq!(
        resolve(&mut p, "p1", PermissionDecision::Allow),
        CasOutcome::Migrated
    );
    let (status, decision) = read_status_and_decision(&p, "p1");
    assert_eq!(status, "resolved");
    assert_eq!(decision, Some(yrs::Out::Any("allow".into())));

    // 重复 resolve → Duplicate（不覆盖）。
    assert_eq!(
        resolve(&mut p, "p1", PermissionDecision::Deny),
        CasOutcome::Duplicate
    );
    let (status, decision) = read_status_and_decision(&p, "p1");
    assert_eq!(status, "resolved");
    assert_eq!(decision, Some(yrs::Out::Any("allow".into())));
}

#[test]
fn resolve_expired_returns_expired() {
    let mut p = pair_with_permission("p1");
    // 先 expire。
    assert_eq!(expire(&mut p, "p1"), CasOutcome::Migrated);
    // resolve 已过期 → Expired，decision 保持 null。
    assert_eq!(
        resolve(&mut p, "p1", PermissionDecision::Allow),
        CasOutcome::Expired
    );
    let (status, decision) = read_status_and_decision(&p, "p1");
    assert_eq!(status, "expired");
    assert_eq!(decision, Some(yrs::Out::Any(yrs::Any::Null)));
}

#[test]
fn expire_pending_keeps_decision_null() {
    let mut p = pair_with_permission("p1");
    assert_eq!(expire(&mut p, "p1"), CasOutcome::Migrated);
    let (status, decision) = read_status_and_decision(&p, "p1");
    assert_eq!(status, "expired");
    assert_eq!(decision, Some(yrs::Out::Any(yrs::Any::Null)));
    // 重复 expire → Duplicate（幂等）。
    assert_eq!(expire(&mut p, "p1"), CasOutcome::Duplicate);
}

#[test]
fn expire_resolved_returns_duplicate() {
    let mut p = pair_with_permission("p1");
    assert_eq!(
        resolve(&mut p, "p1", PermissionDecision::Allow),
        CasOutcome::Migrated
    );
    // 已 resolved 再 expire → Duplicate（不覆盖已裁决）。
    assert_eq!(expire(&mut p, "p1"), CasOutcome::Duplicate);
    let (status, decision) = read_status_and_decision(&p, "p1");
    assert_eq!(status, "resolved");
    assert_eq!(decision, Some(yrs::Out::Any("allow".into())));
}

#[test]
fn unknown_permission_returns_unknown() {
    let mut p = Factory::new().create_chat_doc();
    assert_eq!(
        resolve(&mut p, "nope", PermissionDecision::Allow),
        CasOutcome::Unknown
    );
    assert_eq!(expire(&mut p, "nope"), CasOutcome::Unknown);
}
