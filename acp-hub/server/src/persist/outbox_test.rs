//! outbox 测试（`docs/plans/f3-persist.md` §11：T4/T5/T8 + 主管 H1 裁决路径）。
//!
//! T4 状态机合法/非法迁移；H1 投递后 retryable 失败回退；T5 跨重启重放重建
//! 去重索引；T8 清理策略（7 天保留 + 压缩）。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tempfile::tempdir;

use crate::config::FsyncMode;
use crate::persist::outbox::{
    CommandRecovery, CommandType, DeliveryVerdict, LastError, NewOutboxRecord, OutboxLogEntry,
    OutboxRecord, OutboxStatus, OutboxStore, RetryableClass,
};
use crate::persist::{DegradedFlag, StoreError};

fn test_outbox(dir: &Path, retention: Duration) -> OutboxStore {
    let degraded = Arc::new(DegradedFlag::new());
    OutboxStore::open(dir, FsyncMode::PerCommit, retention, degraded).unwrap()
}

fn new_rec(chat_id: uuid::Uuid, command_type: CommandType) -> NewOutboxRecord {
    NewOutboxRecord {
        command_id: uuid::Uuid::new_v4(),
        chat_id,
        command_type,
        turn_id: None,
        retryable_class: command_type.default_retryable_class(),
    }
}

fn retryable_err() -> LastError {
    LastError {
        code: "AGENT_UNAVAILABLE".into(),
        retryable: true,
        at: Utc::now(),
    }
}

fn fatal_err() -> LastError {
    LastError {
        code: "INVALID_STATE".into(),
        retryable: false,
        at: Utc::now(),
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// T4a：合法迁移全路径（completed 主链 + delivery_unknown 裁决 + 失败路径）。
#[test]
fn t4_legal_transitions_all_green() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    let rec = new_rec(sid, CommandType::Prompt);
    ob.insert(rec.clone()).unwrap();
    assert_eq!(
        ob.get(rec.command_id).unwrap().status,
        OutboxStatus::Received
    );
    ob.mark_accepted(rec.command_id).unwrap();
    assert_eq!(
        ob.get(rec.command_id).unwrap().status,
        OutboxStatus::Accepted
    );
    ob.mark_intent_durable(rec.command_id).unwrap();
    assert_eq!(
        ob.get(rec.command_id).unwrap().status,
        OutboxStatus::IntentDurable
    );
    let at = Utc::now();
    ob.mark_dispatched(rec.command_id, at).unwrap();
    let r = ob.get(rec.command_id).unwrap();
    assert_eq!(r.status, OutboxStatus::Dispatched);
    assert_eq!(r.dispatched_at, Some(at));
    assert_eq!(r.attempt_count, 1);
    ob.mark_delivery_confirmed(rec.command_id).unwrap();
    assert_eq!(
        ob.get(rec.command_id).unwrap().status,
        OutboxStatus::DeliveryConfirmed
    );
    ob.mark_projection_committed(rec.command_id).unwrap();
    assert_eq!(
        ob.get(rec.command_id).unwrap().status,
        OutboxStatus::ProjectionCommitted
    );
    ob.mark_completed(rec.command_id).unwrap();
    assert_eq!(
        ob.get(rec.command_id).unwrap().status,
        OutboxStatus::Completed
    );

    // delivery_unknown → ConfirmedDelivered → completed
    let rec2 = new_rec(sid, CommandType::Prompt);
    ob.insert(rec2.clone()).unwrap();
    ob.mark_accepted(rec2.command_id).unwrap();
    ob.mark_intent_durable(rec2.command_id).unwrap();
    ob.mark_dispatched(rec2.command_id, Utc::now()).unwrap();
    ob.mark_delivery_unknown(rec2.command_id).unwrap();
    assert_eq!(
        ob.get(rec2.command_id).unwrap().status,
        OutboxStatus::DeliveryUnknown
    );
    ob.resolve_delivery_unknown(rec2.command_id, DeliveryVerdict::ConfirmedDelivered)
        .unwrap();
    assert_eq!(
        ob.get(rec2.command_id).unwrap().status,
        OutboxStatus::Completed
    );

    // delivery_unknown → ConfirmedNotDelivered → tombstone
    let rec3 = new_rec(sid, CommandType::Cancel);
    ob.insert(rec3.clone()).unwrap();
    ob.mark_accepted(rec3.command_id).unwrap();
    ob.mark_intent_durable(rec3.command_id).unwrap();
    ob.mark_dispatched(rec3.command_id, Utc::now()).unwrap();
    ob.mark_delivery_unknown(rec3.command_id).unwrap();
    ob.resolve_delivery_unknown(rec3.command_id, DeliveryVerdict::ConfirmedNotDelivered)
        .unwrap();
    assert!(ob.get(rec3.command_id).is_none());

    // delivery_unknown → StillUnknown 幂等保持
    let rec4 = new_rec(sid, CommandType::Cancel);
    ob.insert(rec4.clone()).unwrap();
    ob.mark_accepted(rec4.command_id).unwrap();
    ob.mark_intent_durable(rec4.command_id).unwrap();
    ob.mark_dispatched(rec4.command_id, Utc::now()).unwrap();
    ob.mark_delivery_unknown(rec4.command_id).unwrap();
    ob.resolve_delivery_unknown(rec4.command_id, DeliveryVerdict::StillUnknown)
        .unwrap();
    assert_eq!(
        ob.get(rec4.command_id).unwrap().status,
        OutboxStatus::DeliveryUnknown
    );

    // delivery_confirmed → failed（业务失败）
    let rec5 = new_rec(sid, CommandType::Prompt);
    ob.insert(rec5.clone()).unwrap();
    ob.mark_accepted(rec5.command_id).unwrap();
    ob.mark_intent_durable(rec5.command_id).unwrap();
    ob.mark_dispatched(rec5.command_id, Utc::now()).unwrap();
    ob.mark_delivery_confirmed(rec5.command_id).unwrap();
    ob.mark_failed(rec5.command_id, fatal_err()).unwrap();
    assert_eq!(
        ob.get(rec5.command_id).unwrap().status,
        OutboxStatus::Failed
    );

    // intent_durable → clear_for_retry（retryable 清除）
    let rec6 = new_rec(sid, CommandType::Close);
    ob.insert(rec6.clone()).unwrap();
    ob.mark_accepted(rec6.command_id).unwrap();
    ob.mark_intent_durable(rec6.command_id).unwrap();
    ob.clear_for_retry(rec6.command_id).unwrap();
    assert!(ob.get(rec6.command_id).is_none());
}

#[test]
fn permission_recovery_survives_reopen_and_clears_after_delivery() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let rec = new_rec(sid, CommandType::Resolve);
    let evidence = CommandRecovery::PermissionResponse {
        permission_id: "permission-1".into(),
        request_id: serde_json::json!(42),
        options: vec![serde_json::json!({"optionId":"allow-once","kind":"allow_once"})],
        decision: acp_hub_proto::action::PermissionDecision::Allow,
    };
    {
        let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
        ob.insert(rec.clone()).unwrap();
        ob.mark_accepted(rec.command_id).unwrap();
        ob.mark_intent_durable(rec.command_id).unwrap();
        ob.set_recovery(rec.command_id, evidence.clone()).unwrap();
        ob.mark_dispatched(rec.command_id, Utc::now()).unwrap();
        ob.mark_failed(rec.command_id, retryable_err()).unwrap();
        assert_eq!(
            ob.get(rec.command_id).unwrap().status,
            OutboxStatus::IntentDurable
        );
    }

    let mut recovered = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    let result = recovered.replay_from_disk().unwrap();
    assert!(!result.degraded);
    let record = recovered.get(rec.command_id).unwrap();
    assert_eq!(record.status, OutboxStatus::IntentDurable);
    assert_eq!(record.recovery.as_deref(), Some(&evidence));

    assert_eq!(recovered.reconcile_recovery_after_restart().unwrap(), 1);
    assert_eq!(
        recovered.get(rec.command_id).unwrap().status,
        OutboxStatus::DeliveryUnknown
    );
    assert_eq!(
        recovered.get(rec.command_id).unwrap().recovery.as_deref(),
        Some(&evidence)
    );
}

#[test]
fn permission_recovery_clears_after_confirmed_delivery() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let rec = new_rec(sid, CommandType::Resolve);
    let evidence = CommandRecovery::PermissionResponse {
        permission_id: "permission-1".into(),
        request_id: serde_json::json!(42),
        options: vec![],
        decision: acp_hub_proto::action::PermissionDecision::Deny,
    };
    let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    ob.insert(rec.clone()).unwrap();
    ob.mark_accepted(rec.command_id).unwrap();
    ob.mark_intent_durable(rec.command_id).unwrap();
    ob.set_recovery(rec.command_id, evidence).unwrap();
    ob.mark_dispatched(rec.command_id, Utc::now()).unwrap();
    ob.mark_delivery_confirmed(rec.command_id).unwrap();
    ob.clear_recovery(rec.command_id).unwrap();
    assert!(ob.get(rec.command_id).unwrap().recovery.is_none());
}

#[test]
fn prompt_restart_reconciliation_never_redelivers_across_the_barrier() {
    let dir = tempdir().unwrap();
    let chat_id = uuid::Uuid::new_v4();
    let mut outbox = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));

    let accepted = new_rec(chat_id, CommandType::Prompt);
    outbox.insert(accepted.clone()).unwrap();
    outbox.mark_accepted(accepted.command_id).unwrap();
    outbox
        .set_prompt_payload_fingerprint(accepted.command_id, "accepted-fingerprint".into())
        .unwrap();

    let safe_intent = new_rec(chat_id, CommandType::Prompt);
    outbox.insert(safe_intent.clone()).unwrap();
    outbox.mark_accepted(safe_intent.command_id).unwrap();
    outbox
        .mark_prompt_intent_durable(safe_intent.command_id, "safe-fingerprint".into())
        .unwrap();

    let barrier = new_rec(chat_id, CommandType::Prompt);
    outbox.insert(barrier.clone()).unwrap();
    outbox.mark_accepted(barrier.command_id).unwrap();
    outbox
        .mark_prompt_intent_durable(barrier.command_id, "barrier-fingerprint".into())
        .unwrap();
    outbox
        .mark_dispatch_barrier(barrier.command_id, Utc::now())
        .unwrap();

    let legacy = new_rec(chat_id, CommandType::Prompt);
    outbox.insert(legacy.clone()).unwrap();
    outbox.mark_accepted(legacy.command_id).unwrap();
    outbox.mark_intent_durable(legacy.command_id).unwrap();

    let projected = new_rec(chat_id, CommandType::Prompt);
    outbox.insert(projected.clone()).unwrap();
    outbox.mark_accepted(projected.command_id).unwrap();
    outbox.mark_intent_durable(projected.command_id).unwrap();
    outbox
        .mark_dispatched(projected.command_id, Utc::now())
        .unwrap();
    outbox
        .mark_delivery_confirmed(projected.command_id)
        .unwrap();
    outbox
        .mark_projection_committed(projected.command_id)
        .unwrap();

    let repairable = new_rec(chat_id, CommandType::Prompt);
    outbox.insert(repairable.clone()).unwrap();
    outbox.mark_accepted(repairable.command_id).unwrap();
    outbox
        .mark_prompt_intent_durable(repairable.command_id, "repair-fingerprint".into())
        .unwrap();
    outbox
        .mark_dispatch_barrier(repairable.command_id, Utc::now())
        .unwrap();
    outbox
        .mark_dispatched(repairable.command_id, Utc::now())
        .unwrap();
    outbox
        .mark_delivery_confirmed(repairable.command_id)
        .unwrap();

    let terminal = std::collections::HashSet::from([repairable.command_id]);

    assert_eq!(
        outbox
            .reconcile_prompt_delivery_after_restart(&terminal)
            .unwrap(),
        6
    );
    assert_eq!(
        outbox.get(accepted.command_id).unwrap().status,
        OutboxStatus::Failed
    );
    assert_eq!(
        outbox.get(safe_intent.command_id).unwrap().status,
        OutboxStatus::Failed
    );
    for id in [barrier.command_id, legacy.command_id, projected.command_id] {
        let record = outbox.get(id).unwrap();
        assert_eq!(record.status, OutboxStatus::DeliveryUnknown);
        assert_eq!(record.last_error.as_ref().unwrap().code, "DELIVERY_UNKNOWN");
        assert!(!record.last_error.as_ref().unwrap().retryable);
    }
    assert_eq!(
        outbox.get(repairable.command_id).unwrap().status,
        OutboxStatus::Completed
    );
}

#[test]
fn legacy_outbox_record_without_recovery_field_still_decodes() {
    let command_id = uuid::Uuid::new_v4();
    let chat_id = uuid::Uuid::new_v4();
    let now = Utc::now();
    let legacy = serde_json::json!({
        "commandId": command_id,
        "chatId": chat_id,
        "commandType": "chat/prompt",
        "turnId": null,
        "status": "completed",
        "retryableClass": "no_auto_redeliver",
        "dispatchedAt": now,
        "createdAt": now,
        "updatedAt": now,
        "lastError": null,
        "attemptCount": 1
    });
    let record: OutboxRecord = serde_json::from_value(legacy).unwrap();
    assert_eq!(record.command_id, command_id);
    assert!(record.recovery.is_none());
}

/// T4b：非法迁移拒绝 + 文件无新增记录。
#[test]
fn t4_illegal_transitions_rejected() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    // —— 合法 setup（先全部走完，之后测量文件基线）——
    let rec = new_rec(sid, CommandType::Prompt);
    ob.insert(rec.clone()).unwrap();
    // 走完主链到 completed
    ob.mark_accepted(rec.command_id).unwrap();
    ob.mark_intent_durable(rec.command_id).unwrap();
    ob.mark_dispatched(rec.command_id, Utc::now()).unwrap();
    ob.mark_delivery_confirmed(rec.command_id).unwrap();
    ob.mark_projection_committed(rec.command_id).unwrap();
    ob.mark_completed(rec.command_id).unwrap();
    // 独立记录（合法走到 dispatched）
    let rec2 = new_rec(sid, CommandType::Prompt);
    ob.insert(rec2.clone()).unwrap();
    ob.mark_accepted(rec2.command_id).unwrap();
    ob.mark_intent_durable(rec2.command_id).unwrap();
    ob.mark_dispatched(rec2.command_id, Utc::now()).unwrap();
    // delivery_unknown 记录
    let rec3 = new_rec(sid, CommandType::Cancel);
    ob.insert(rec3.clone()).unwrap();
    ob.mark_accepted(rec3.command_id).unwrap();
    ob.mark_intent_durable(rec3.command_id).unwrap();
    ob.mark_dispatched(rec3.command_id, Utc::now()).unwrap();
    ob.mark_delivery_unknown(rec3.command_id).unwrap();
    // accepted 状态记录
    let rec4 = new_rec(sid, CommandType::Prompt);
    ob.insert(rec4.clone()).unwrap();
    ob.mark_accepted(rec4.command_id).unwrap();
    let before = file_len(&dir.path().join("outbox.log"));

    // —— 非法迁移（全部拒绝，不落盘）——
    // 终态 → 任何状态
    assert!(matches!(
        ob.mark_accepted(rec.command_id),
        Err(StoreError::InvalidTransition { .. })
    ));
    assert!(matches!(
        ob.mark_dispatched(rec.command_id, Utc::now()),
        Err(StoreError::InvalidTransition { .. })
    ));
    assert!(matches!(
        ob.mark_failed(rec.command_id, retryable_err()),
        Err(StoreError::InvalidTransition { .. })
    ));
    assert!(matches!(
        ob.resolve_delivery_unknown(rec.command_id, DeliveryVerdict::ConfirmedDelivered),
        Err(StoreError::InvalidTransition { .. })
    ));

    // 独立记录：跳过状态（received → dispatched）
    assert!(matches!(
        ob.mark_dispatched(rec2.command_id, Utc::now()),
        Err(StoreError::InvalidTransition { .. })
    ));
    assert!(matches!(
        ob.mark_projection_committed(rec2.command_id),
        Err(StoreError::InvalidTransition { .. })
    ));
    // 非法：dispatched → projection_committed（跳过确认）
    assert!(matches!(
        ob.mark_projection_committed(rec2.command_id),
        Err(StoreError::InvalidTransition { .. })
    ));
    // 非法：delivery_unknown → dispatched（非幂等禁止自动重发）
    assert!(matches!(
        ob.mark_dispatched(rec3.command_id, Utc::now()),
        Err(StoreError::InvalidTransition { .. })
    ));
    // 非法：delivery_unknown 直接 mark_failed（须走裁决）
    assert!(matches!(
        ob.mark_failed(rec3.command_id, fatal_err()),
        Err(StoreError::InvalidTransition { .. })
    ));
    // 非法：clear_for_retry 对非 intent_durable/delivery_unknown 状态
    assert!(matches!(
        ob.clear_for_retry(rec4.command_id),
        Err(StoreError::InvalidTransition { .. })
    ));
    // 重复 insert（重发穿透防护）
    assert!(matches!(
        ob.insert(rec.clone()),
        Err(StoreError::DuplicateCommand { .. })
    ));
    // 不存在的 commandId
    assert!(matches!(
        ob.mark_accepted(uuid::Uuid::new_v4()),
        Err(StoreError::CommandNotFound { .. })
    ));
    // 所有非法迁移均不落盘
    let after = file_len(&dir.path().join("outbox.log"));
    assert_eq!(before, after, "illegal transitions must not append records");
}

/// H1 裁决：投递后 retryable 失败 → 回退 intent_durable（记录保留、索引不删、
/// dispatched_at 清除、可重发）；重发 attempt_count 递增。
#[test]
fn h1_delivery_confirmed_retryable_failure_falls_back() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    let rec = new_rec(sid, CommandType::Prompt);
    ob.insert(rec.clone()).unwrap();
    ob.mark_accepted(rec.command_id).unwrap();
    ob.mark_intent_durable(rec.command_id).unwrap();
    ob.mark_dispatched(rec.command_id, Utc::now()).unwrap();
    ob.mark_delivery_confirmed(rec.command_id).unwrap();
    // 投递后 retryable 失败（如 AGENT_UNAVAILABLE）
    ob.mark_failed(rec.command_id, retryable_err()).unwrap();
    let r = ob.get(rec.command_id).expect("record must be kept");
    assert_eq!(
        r.status,
        OutboxStatus::IntentDurable,
        "fallback to intent_durable"
    );
    assert_eq!(r.dispatched_at, None, "dispatch bit cleared");
    assert_eq!(r.last_error.as_ref().unwrap().code, "AGENT_UNAVAILABLE");
    // 可重发：再次投递
    ob.mark_dispatched(rec.command_id, Utc::now()).unwrap();
    let r2 = ob.get(rec.command_id).unwrap();
    assert_eq!(r2.status, OutboxStatus::Dispatched);
    assert_eq!(r2.attempt_count, 2, "attempt count observable");
}

/// H1 裁决：投递前 retryable 失败 → tombstone 清除（设计稿 §5.2 原语义）；
/// 非 retryable 失败 → failed 终态。
#[test]
fn h1_pre_dispatch_retryable_clears_and_fatal_fails() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    // 投递前 retryable → 清除
    let rec = new_rec(sid, CommandType::Prompt);
    ob.insert(rec.clone()).unwrap();
    ob.mark_accepted(rec.command_id).unwrap();
    ob.mark_intent_durable(rec.command_id).unwrap();
    ob.mark_failed(rec.command_id, retryable_err()).unwrap();
    assert!(ob.get(rec.command_id).is_none(), "cleared for retry");
    // 投递后非 retryable → failed 终态
    let rec2 = new_rec(sid, CommandType::Prompt);
    ob.insert(rec2.clone()).unwrap();
    ob.mark_accepted(rec2.command_id).unwrap();
    ob.mark_intent_durable(rec2.command_id).unwrap();
    ob.mark_dispatched(rec2.command_id, Utc::now()).unwrap();
    ob.mark_delivery_confirmed(rec2.command_id).unwrap();
    ob.mark_failed(rec2.command_id, fatal_err()).unwrap();
    let r = ob.get(rec2.command_id).unwrap();
    assert_eq!(r.status, OutboxStatus::Failed);
    assert!(!r.last_error.as_ref().unwrap().retryable);
    // 非 retryable 失败对投递前 → failed（不删除）
    let rec3 = new_rec(sid, CommandType::Cancel);
    ob.insert(rec3.clone()).unwrap();
    ob.mark_accepted(rec3.command_id).unwrap();
    ob.mark_failed(rec3.command_id, fatal_err()).unwrap();
    assert_eq!(
        ob.get(rec3.command_id).unwrap().status,
        OutboxStatus::Failed
    );
}

/// T5：跨重启（新实例重放同一目录）重建去重索引；dispatched/delivery_unknown
/// 保留；tombstone 生效。
#[test]
fn t5_restart_replay_rebuilds_index() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let (a, b, c, d, f) = {
        let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
        // 主链完成
        let a = new_rec(sid, CommandType::Prompt);
        ob.insert(a.clone()).unwrap();
        ob.mark_accepted(a.command_id).unwrap();
        ob.mark_intent_durable(a.command_id).unwrap();
        ob.mark_dispatched(a.command_id, Utc::now()).unwrap();
        ob.mark_delivery_confirmed(a.command_id).unwrap();
        ob.mark_projection_committed(a.command_id).unwrap();
        ob.mark_completed(a.command_id).unwrap();
        // dispatched（未完成，保留）
        let b = new_rec(sid, CommandType::Prompt);
        ob.insert(b.clone()).unwrap();
        ob.mark_accepted(b.command_id).unwrap();
        ob.mark_intent_durable(b.command_id).unwrap();
        ob.mark_dispatched(b.command_id, Utc::now()).unwrap();
        // delivery_unknown（保留，供裁决）
        let c = new_rec(sid, CommandType::Cancel);
        ob.insert(c.clone()).unwrap();
        ob.mark_accepted(c.command_id).unwrap();
        ob.mark_intent_durable(c.command_id).unwrap();
        ob.mark_dispatched(c.command_id, Utc::now()).unwrap();
        ob.mark_delivery_unknown(c.command_id).unwrap();
        // failed（终态，保留至清理）
        let d = new_rec(sid, CommandType::Cancel);
        ob.insert(d.clone()).unwrap();
        ob.mark_accepted(d.command_id).unwrap();
        ob.mark_intent_durable(d.command_id).unwrap();
        ob.mark_dispatched(d.command_id, Utc::now()).unwrap();
        ob.mark_delivery_confirmed(d.command_id).unwrap();
        ob.mark_failed(d.command_id, fatal_err()).unwrap();
        // tombstone 删除
        let e = new_rec(sid, CommandType::Close);
        ob.insert(e.clone()).unwrap();
        ob.mark_accepted(e.command_id).unwrap();
        ob.mark_intent_durable(e.command_id).unwrap();
        ob.clear_for_retry(e.command_id).unwrap();
        // H1 回退记录（重放后应为 intent_durable）
        let f = new_rec(sid, CommandType::Prompt);
        ob.insert(f.clone()).unwrap();
        ob.mark_accepted(f.command_id).unwrap();
        ob.mark_intent_durable(f.command_id).unwrap();
        ob.mark_dispatched(f.command_id, Utc::now()).unwrap();
        ob.mark_delivery_confirmed(f.command_id).unwrap();
        ob.mark_failed(f.command_id, retryable_err()).unwrap();
        (
            a.command_id,
            b.command_id,
            c.command_id,
            d.command_id,
            f.command_id,
        )
        // drop = 模拟重启
    };
    // 新实例重放同一目录
    let mut ob2 = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    let result = ob2.replay_from_disk().unwrap();
    assert!(!result.degraded);
    assert!(result.truncated.is_none());
    assert_eq!(
        result.stats.inserted, 6,
        "a/b/c/d/e/f inserted, e tombstoned"
    );
    assert_eq!(result.stats.removed, 1);
    assert_eq!(ob2.len(), 5);
    assert_eq!(ob2.get(a).unwrap().status, OutboxStatus::Completed);
    assert_eq!(ob2.get(b).unwrap().status, OutboxStatus::Dispatched);
    assert_eq!(ob2.get(c).unwrap().status, OutboxStatus::DeliveryUnknown);
    assert_eq!(ob2.get(d).unwrap().status, OutboxStatus::Failed);
    // H1 回退状态跨重启保持（重放后者覆盖前者）
    assert_eq!(ob2.get(f).unwrap().status, OutboxStatus::IntentDurable);
    assert_eq!(ob2.get(f).unwrap().dispatched_at, None);
    assert!(ob2.get(f).unwrap().last_error.is_some());
    assert!(ob2.get(uuid::Uuid::nil()).is_none());
}

/// T8：清理策略——7 天保留期届满 + chat_closed → 终态删除 + 压缩；
/// 未过期/非终态/未关闭 → 保留。
#[test]
fn t8_cleanup_retention_and_compact() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    let now = Utc::now();
    // 终态记录（completed / failed）
    let a = new_rec(sid, CommandType::Prompt);
    ob.insert(a.clone()).unwrap();
    ob.mark_accepted(a.command_id).unwrap();
    ob.mark_intent_durable(a.command_id).unwrap();
    ob.mark_dispatched(a.command_id, Utc::now()).unwrap();
    ob.mark_delivery_confirmed(a.command_id).unwrap();
    ob.mark_projection_committed(a.command_id).unwrap();
    ob.mark_completed(a.command_id).unwrap();
    let b = new_rec(sid, CommandType::Cancel);
    ob.insert(b.clone()).unwrap();
    ob.mark_accepted(b.command_id).unwrap();
    ob.mark_intent_durable(b.command_id).unwrap();
    ob.mark_dispatched(b.command_id, Utc::now()).unwrap();
    ob.mark_failed(b.command_id, fatal_err()).unwrap();
    // 非终态（dispatched）与 delivery_unknown 记录
    let c = new_rec(sid, CommandType::Prompt);
    ob.insert(c.clone()).unwrap();
    ob.mark_accepted(c.command_id).unwrap();
    ob.mark_intent_durable(c.command_id).unwrap();
    ob.mark_dispatched(c.command_id, Utc::now()).unwrap();
    let d = new_rec(sid, CommandType::Cancel);
    ob.insert(d.clone()).unwrap();
    ob.mark_accepted(d.command_id).unwrap();
    ob.mark_intent_durable(d.command_id).unwrap();
    ob.mark_dispatched(d.command_id, Utc::now()).unwrap();
    ob.mark_delivery_unknown(d.command_id).unwrap();

    // chat 未关闭 → 不清理
    let stats = ob.cleanup(now + chrono::Duration::days(30), false);
    assert_eq!(stats.removed, 0);
    assert!(!stats.compressed);
    // 未过保留期 → 不清理
    let stats = ob.cleanup(now + chrono::Duration::days(1), true);
    assert_eq!(stats.removed, 0);
    // 保留期届满 + 关闭 → 终态删除 + 压缩；非终态保留
    let stats = ob.cleanup(now + chrono::Duration::days(30), true);
    assert_eq!(stats.removed, 2, "a completed + b failed removed");
    assert!(stats.compressed);
    assert!(stats.bytes_after < stats.bytes_before);
    assert!(ob.get(a.command_id).is_none());
    assert!(ob.get(b.command_id).is_none());
    assert!(ob.get(c.command_id).is_some(), "non-terminal kept");
    assert!(ob.get(d.command_id).is_some(), "delivery_unknown kept");
    // 压缩后重放：索引一致（文件里只有存活记录）
    let mut ob2 = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    let result = ob2.replay_from_disk().unwrap();
    assert!(!result.degraded);
    assert_eq!(ob2.len(), 2);
    assert!(ob2.get(c.command_id).is_some());
    assert!(ob2.get(d.command_id).is_some());
}

/// T8 补充：重放条目（纯内存 replay API）顺序应用。
#[test]
fn t8_replay_entries_apply_in_order() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    let id = uuid::Uuid::new_v4();
    let rec = |status: OutboxStatus| OutboxRecord {
        command_id: id,
        chat_id: sid,
        command_type: CommandType::Prompt,
        turn_id: None,
        status,
        retryable_class: RetryableClass::NoAutoRedeliver,
        dispatched_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_error: None,
        attempt_count: 1,
        delivery_protocol_version: None,
        payload_fingerprint: None,
        dispatch_barrier_at: None,
        recovery: None,
    };
    let stats = ob.replay([
        OutboxLogEntry::Record(rec(OutboxStatus::Accepted)),
        OutboxLogEntry::Record(rec(OutboxStatus::Dispatched)),
        OutboxLogEntry::Remove(id),
        OutboxLogEntry::Record(rec(OutboxStatus::IntentDurable)),
    ]);
    assert_eq!(
        stats.inserted, 2,
        "Accepted insert + IntentDurable re-insert after Remove"
    );
    assert_eq!(stats.updated, 1, "Dispatched overwrites Accepted");
    assert_eq!(stats.removed, 1);
    assert_eq!(ob.get(id).unwrap().status, OutboxStatus::IntentDurable);
}

#[test]
fn prompt_v2_barrier_is_additive_durable_and_never_retryable() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let mut ob = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    let rec = new_rec(sid, CommandType::Prompt);
    ob.insert(rec.clone()).unwrap();
    ob.mark_accepted(rec.command_id).unwrap();
    ob.mark_prompt_intent_durable(rec.command_id, "sha256:abc".into())
        .unwrap();
    let barrier = Utc::now();
    ob.mark_dispatch_barrier(rec.command_id, barrier).unwrap();

    let current = ob.get(rec.command_id).unwrap();
    assert_eq!(current.status, OutboxStatus::IntentDurable);
    assert_eq!(current.delivery_protocol_version, Some(2));
    assert_eq!(current.payload_fingerprint.as_deref(), Some("sha256:abc"));
    assert_eq!(current.dispatch_barrier_at, Some(barrier));

    ob.mark_failed(rec.command_id, retryable_err()).unwrap();
    assert_eq!(
        ob.get(rec.command_id).unwrap().status,
        OutboxStatus::DeliveryUnknown
    );

    let mut reopened = test_outbox(dir.path(), Duration::from_secs(7 * 86_400));
    reopened.replay_from_disk().unwrap();
    let durable = reopened.get(rec.command_id).unwrap();
    assert_eq!(durable.status, OutboxStatus::DeliveryUnknown);
    assert_eq!(durable.delivery_protocol_version, Some(2));
    assert_eq!(durable.payload_fingerprint.as_deref(), Some("sha256:abc"));
    assert_eq!(durable.dispatch_barrier_at, Some(barrier));
}

#[test]
fn legacy_record_without_v2_fields_still_decodes() {
    let id = uuid::Uuid::new_v4();
    let chat = uuid::Uuid::new_v4();
    let json = serde_json::json!({
        "commandId": id,
        "chatId": chat,
        "commandType": "chat/prompt",
        "turnId": null,
        "status": "intent_durable",
        "retryableClass": "no_auto_redeliver",
        "dispatchedAt": null,
        "createdAt": Utc::now(),
        "updatedAt": Utc::now(),
        "lastError": null,
        "attemptCount": 0
    });
    let record: OutboxRecord = serde_json::from_value(json).unwrap();
    assert_eq!(record.delivery_protocol_version, None);
    assert_eq!(record.payload_fingerprint, None);
    assert_eq!(record.dispatch_barrier_at, None);
}
