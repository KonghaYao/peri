//! Store 集成测试（`docs/plans/f3-persist.md` §11：T7/T9/T10/T11 + 快照失效）。
//!
//! T7 compact 原子性（崩溃时序 A/B/C）；T9 磁盘预算；T10 恢复编排集成
//! （多 session 混合状态 → RecoveryResult 聚合）；T11 目录权限（unix）。

use std::path::PathBuf;
use std::time::Duration;

use acp_hub_proto::conn::DocId;
use tempfile::tempdir;

use crate::config::FsyncMode;
use crate::persist::outbox::{CommandType, DeliveryVerdict};
use crate::persist::store::{EvictionCandidate, Store};
use crate::persist::{
    PersistConfig, RecoveryResult, StoreError, WarningCode,
};

/// 测试配置：tempdir 数据目录 + PerCommit + 大 compact 阈值（默认不触发）。
fn test_config(data_dir: PathBuf) -> PersistConfig {
    PersistConfig {
        data_dir,
        fsync_mode: FsyncMode::PerCommit,
        compact_threshold_bytes: 64 * 1024 * 1024,
        compact_interval: Duration::from_secs(24 * 3600),
        disk_budget: 2 * 1024 * 1024 * 1024,
        outbox_retention: Duration::from_secs(7 * 86_400),
        archive_retention: Duration::from_secs(90 * 86_400),
    }
}

fn chat_doc(sid: &uuid::Uuid, payload: &[u8]) -> (DocId, Vec<u8>) {
    (DocId::chat(&sid.to_string()), payload.to_vec())
}

/// 构造一个已 append 一条记录 + outbox 终态记录的 session（T9 候选构造）。
async fn seed_session_with_terminal(store: &Store, sid: uuid::Uuid) {
    let session = store.create_session(sid).unwrap();
    let d = chat_doc(&sid, b"seed");
    session
        .append_update(1, 1, &[(d.0.clone(), &d.1)])
        .await
        .unwrap();
    let rec = crate::persist::outbox::NewOutboxRecord {
        command_id: uuid::Uuid::new_v4(),
        session_id: sid,
        command_type: CommandType::Prompt,
        turn_id: None,
        retryable_class: crate::persist::outbox::RetryableClass::NoAutoRedeliver,
    };
    {
        let mut ob = session.outbox().lock().await;
        ob.insert(rec.clone()).unwrap();
        ob.mark_accepted(rec.command_id).unwrap();
        ob.mark_intent_durable(rec.command_id).unwrap();
        ob.mark_dispatched(rec.command_id, chrono::Utc::now()).unwrap();
        ob.mark_delivery_confirmed(rec.command_id).unwrap();
        ob.mark_projection_committed(rec.command_id).unwrap();
        ob.mark_completed(rec.command_id).unwrap();
    }
    session.mark_closed(chrono::Utc::now());
}

/// T7-A：tmp 残留（rename 前崩溃）→ recover 删 tmp + 纯日志回放。
#[tokio::test]
async fn t7_crash_a_tmp_leftover_cleaned() {
    let dir = tempdir().unwrap();
    let cfg = test_config(dir.path().to_path_buf());
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload");
    {
        let store = Store::open(&cfg).unwrap();
        let session = store.create_session(sid).unwrap();
        session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
        session.append_update(1, 2, &[(d.0.clone(), &d.1)]).await.unwrap();
    }
    // 手动制造 tmp 残留（模拟 rename 前崩溃，旧日志完整）
    let tmp = dir.path().join("sessions").join(sid.to_string()).join("updates.snapshot.tmp");
    std::fs::write(&tmp, b"stale").unwrap();
    let store = Store::open(&cfg).unwrap();
    let result = store.recover().await;
    assert!(!result.degraded, "A must recover cleanly");
    assert!(!tmp.exists(), "stale tmp removed");
    let replay = store.replay_outcome(sid).unwrap();
    assert_eq!(replay.records.len(), 2, "pure log replay");
    assert!(replay.snapshot.is_none());
    assert_eq!(replay.watermark.last_seq, 2);
    assert!(store.is_recovered());
    assert!(store.last_recovery().is_some());
}

/// T7-B：快照 + 重复日志（rename 后、truncate 前崩溃）→ 快照基线 + 日志截断。
#[tokio::test]
async fn t7_crash_b_snapshot_base_with_dup_log() {
    let dir = tempdir().unwrap();
    let mut cfg = test_config(dir.path().to_path_buf());
    cfg.compact_threshold_bytes = 1; // 立即触发
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload");
    let snapshot_docs = {
        let store = Store::open(&cfg).unwrap();
        let session = store.create_session(sid).unwrap();
        session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
        session.append_update(1, 2, &[(d.0.clone(), &d.1)]).await.unwrap();
        session.append_update(1, 3, &[(d.0.clone(), &d.1)]).await.unwrap();
        let docs = std::collections::HashMap::from([(d.0.clone(), d.1.clone())]);
        let mut lg = session.update_log().lock().await;
        lg.compact(docs.clone()).await.unwrap();
        // 模拟 rename 后崩溃残留：追加一条 ≤ 快照点的重复记录（seq 2）
        let d_ref = (d.0.clone(), d.1.as_slice());
        drop(lg);
        session.append_update(1, 2, &[d_ref]).await.unwrap();
        docs
    };
    let store = Store::open(&cfg).unwrap();
    let result = store.recover().await;
    assert!(!result.degraded, "B recovers with snapshot base");
    let replay = store.replay_outcome(sid).unwrap();
    let snap = replay.snapshot.expect("snapshot base present");
    assert_eq!(snap.last_applied_seq, 3);
    assert_eq!(snap.docs, snapshot_docs);
    // 重复日志段被截断（快照点 ≥ 日志尾部 → 截断日志）
    assert!(replay.records.is_empty(), "dup log truncated");
    let log_path = dir
        .path()
        .join("sessions")
        .join(sid.to_string())
        .join("updates.log");
    assert_eq!(std::fs::metadata(&log_path).unwrap().len(), 0, "log truncated");
}

/// T7-C：快照 + 空日志（truncate 后崩溃）→ 快照基线。
#[tokio::test]
async fn t7_crash_c_snapshot_only() {
    let dir = tempdir().unwrap();
    let cfg = test_config(dir.path().to_path_buf());
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload");
    {
        let store = Store::open(&cfg).unwrap();
        let session = store.create_session(sid).unwrap();
        session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
        session.append_update(1, 2, &[(d.0.clone(), &d.1)]).await.unwrap();
        let docs = std::collections::HashMap::from([(d.0.clone(), d.1.clone())]);
        let mut lg = session.update_log().lock().await;
        lg.compact(docs).await.unwrap();
    }
    let store = Store::open(&cfg).unwrap();
    let result = store.recover().await;
    assert!(!result.degraded);
    let replay = store.replay_outcome(sid).unwrap();
    assert!(replay.records.is_empty());
    let snap = replay.snapshot.unwrap();
    assert_eq!(snap.last_applied_seq, 2);
    assert_eq!(replay.watermark.last_seq, 2);
}

/// T7-触发：maybe_compact 大小阈值与间隔阈值。
#[tokio::test]
async fn t7_maybe_compact_trigger() {
    let dir = tempdir().unwrap();
    let mut cfg = test_config(dir.path().to_path_buf());
    cfg.compact_threshold_bytes = 1; // 任意 append 即超阈值
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload");
    let store = Store::open(&cfg).unwrap();
    let session = store.create_session(sid).unwrap();
    session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
    let docs = std::collections::HashMap::from([(d.0.clone(), d.1.clone())]);
    let mut lg = session.update_log().lock().await;
    let triggered = lg.maybe_compact(docs.clone()).await.unwrap();
    assert!(triggered, "size threshold triggers");
    assert_eq!(lg.stats().records, 0, "log truncated after compact");
    // 再次触发（间隔未到、日志空 → 不触发；阈值已满足 bytes=0 → false）
    let triggered2 = lg.maybe_compact(docs).await.unwrap();
    assert!(!triggered2);
    drop(lg);
    // 间隔触发：compact_interval = 0（恒过期）
    let mut cfg2 = test_config(dir.path().to_path_buf());
    cfg2.compact_threshold_bytes = 64 * 1024 * 1024;
    cfg2.compact_interval = Duration::ZERO;
    let store2 = Store::open(&cfg2).unwrap();
    let session2 = store2.create_session(uuid::Uuid::new_v4()).unwrap();
    session2.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
    let mut lg2 = session2.update_log().lock().await;
    let triggered3 = lg2
        .maybe_compact(std::collections::HashMap::from([(d.0, d.1)]))
        .await
        .unwrap();
    // 无快照（last_compact_at=None）→ 间隔条件不成立；大小阈值未超 → false
    assert!(!triggered3);
}

/// T7-快照失效：快照 CRC/解析失败 → 移 corrupt/ + degraded + 纯日志回放。
#[tokio::test]
async fn t7_invalid_snapshot_moved_to_corrupt() {
    let dir = tempdir().unwrap();
    let cfg = test_config(dir.path().to_path_buf());
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload");
    {
        let store = Store::open(&cfg).unwrap();
        let session = store.create_session(sid).unwrap();
        session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
        let docs = std::collections::HashMap::from([(d.0.clone(), d.1.clone())]);
        let mut lg = session.update_log().lock().await;
        lg.compact(docs).await.unwrap();
        // compact 后补一条（纯日志回放的增量）
        let d_ref = (d.0.clone(), d.1.as_slice());
        drop(lg);
        session.append_update(1, 2, &[d_ref]).await.unwrap();
    }
    // 破坏快照文件
    let snap = dir
        .path()
        .join("sessions")
        .join(sid.to_string())
        .join("updates.snapshot");
    std::fs::write(&snap, b"garbage").unwrap();
    let store = Store::open(&cfg).unwrap();
    let result = store.recover().await;
    assert!(result.degraded, "invalid snapshot => degraded");
    assert!(result
        .warnings
        .iter()
        .any(|w| w.code == WarningCode::SnapshotInvalid));
    let corrupt_dir = dir
        .path()
        .join("sessions")
        .join(sid.to_string())
        .join("corrupt");
    let artifacts: Vec<String> = std::fs::read_dir(&corrupt_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        artifacts.iter().any(|a| a.contains("snapshot.invalid")),
        "invalid snapshot preserved in corrupt/: {artifacts:?}"
    );
    // 纯日志回放：seq 2 记录
    let replay = store.replay_outcome(sid).unwrap();
    assert!(replay.snapshot.is_none());
    assert_eq!(replay.records.len(), 1);
    assert_eq!(replay.records[0].seq, 2);
}

/// T9a：预算超限无候选 → 告警 + degraded。
#[tokio::test]
async fn t9_budget_exceeded_no_candidates_degrades() {
    let dir = tempdir().unwrap();
    let mut cfg = test_config(dir.path().to_path_buf());
    cfg.disk_budget = 64; // 极小预算
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload-01");
    let store = Store::open(&cfg).unwrap();
    let session = store.create_session(sid).unwrap();
    session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
    let report = store.enforce_budget();
    assert!(report.exceeded);
    assert!(report.used > 0);
    assert!(report.eviction_candidates.is_empty(), "no candidates yet");
    assert!(store.status().degraded, "no eviction candidate => degraded");
}

/// T9b：预算超限有候选（已关闭 session + 终态记录）→ 候选列出，不置 degraded。
#[tokio::test]
async fn t9_budget_exceeded_with_candidates() {
    let dir = tempdir().unwrap();
    let mut cfg = test_config(dir.path().to_path_buf());
    cfg.disk_budget = 64;
    let sid = uuid::Uuid::new_v4();
    let store = Store::open(&cfg).unwrap();
    seed_session_with_terminal(&store, sid).await;
    let report = store.enforce_budget();
    assert!(report.exceeded);
    assert!(report.eviction_candidates.contains(
        &EvictionCandidate::ArchiveSession { session_id: sid }
    ));
    assert!(report.eviction_candidates.iter().any(
        |c| matches!(c, EvictionCandidate::OutboxTerminal { session_id: s, .. } if *s == sid)
    ));
    assert!(
        !store.status().degraded,
        "candidates available => not degraded (M1 只告警+候选)"
    );
}

/// T10：恢复编排集成——多 session 混合状态（截断日志 + delivery_unknown +
/// 水位落后）→ RecoveryResult 汇总正确。
#[tokio::test]
async fn t10_recovery_orchestration_integration() {
    let dir = tempdir().unwrap();
    let cfg = test_config(dir.path().to_path_buf());
    let sid_a = uuid::Uuid::new_v4();
    let sid_b = uuid::Uuid::new_v4();
    let d_a = chat_doc(&sid_a, b"payload-a");
    let d_b = chat_doc(&sid_b, b"payload-b");
    let unknown_cmd = {
        let store = Store::open(&cfg).unwrap();
        // session A：3 条 update + outbox 到 delivery_unknown
        let a = store.create_session(sid_a).unwrap();
        a.append_update(1, 1, &[(d_a.0.clone(), &d_a.1)]).await.unwrap();
        a.append_update(1, 2, &[(d_a.0.clone(), &d_a.1)]).await.unwrap();
        a.append_update(1, 3, &[(d_a.0.clone(), &d_a.1)]).await.unwrap();
        let rec = crate::persist::outbox::NewOutboxRecord {
            command_id: uuid::Uuid::new_v4(),
            session_id: sid_a,
            command_type: CommandType::Prompt,
            turn_id: None,
            retryable_class: crate::persist::outbox::RetryableClass::NoAutoRedeliver,
        };
        {
            let mut ob = a.outbox().lock().await;
            ob.insert(rec.clone()).unwrap();
            ob.mark_accepted(rec.command_id).unwrap();
            ob.mark_intent_durable(rec.command_id).unwrap();
            ob.mark_dispatched(rec.command_id, chrono::Utc::now()).unwrap();
            ob.mark_delivery_unknown(rec.command_id).unwrap();
        }
        // session B：正常 2 条
        let b = store.create_session(sid_b).unwrap();
        b.append_update(1, 1, &[(d_b.0.clone(), &d_b.1)]).await.unwrap();
        b.append_update(1, 2, &[(d_b.0.clone(), &d_b.1)]).await.unwrap();
        drop(store);
        // 破坏 session A 的第 2 条 update 记录 payload
        let log_path = dir
            .path()
            .join("sessions")
            .join(sid_a.to_string())
            .join("updates.log");
        let data = std::fs::read(&log_path).unwrap();
        let len1 = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        let second_payload = 8 + len1 + 8;
        let mut corrupted = data;
        corrupted[second_payload] ^= 0xFF;
        std::fs::write(&log_path, &corrupted).unwrap();
        rec.command_id
    };
    // 重启恢复
    let store = Store::open(&cfg).unwrap();
    let result: RecoveryResult = store.recover().await;
    // 聚合：degraded + TailTruncated + 字节统计 + corrupt 段
    assert!(result.degraded, "truncated log => degraded");
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.code == WarningCode::TailTruncated),
        "warnings: {:?}",
        result.warnings
    );
    assert!(result.truncated_total_bytes > 0);
    assert!(
        result
            .corrupt_artifacts
            .iter()
            .any(|p| p.to_string_lossy().contains("updates.log")),
        "corrupt artifacts: {:?}",
        result.corrupt_artifacts
    );
    // session A：截断后 1 条 + 水位对齐 min(3, 1) + SeqMismatch 告警
    let ra = store.replay_outcome(sid_a).unwrap();
    assert_eq!(ra.records.len(), 1);
    assert_eq!(ra.records[0].seq, 1);
    assert_eq!(ra.watermark.last_seq, 1, "min(watermark 3, log 1)");
    assert!(result
        .warnings
        .iter()
        .any(|w| w.code == WarningCode::SeqMismatch));
    // session A：outbox delivery_unknown 保留
    let a = store.session(sid_a).unwrap();
    let cmd = a.outbox_get(unknown_cmd).await.unwrap();
    assert_eq!(
        cmd.status,
        crate::persist::outbox::OutboxStatus::DeliveryUnknown,
        "delivery_unknown survives restart"
    );
    // session B：正常，无告警归属
    let rb = store.replay_outcome(sid_b).unwrap();
    assert_eq!(rb.records.len(), 2);
    assert_eq!(rb.watermark.last_seq, 2);
    // 幂等：第二次 recover 返回相同结果
    let again = store.recover().await;
    assert_eq!(again.degraded, result.degraded);
    assert_eq!(again.truncated_total_bytes, result.truncated_total_bytes);
    assert_eq!(again.warnings.len(), result.warnings.len());
    // recover 完成信号已通知（is_recovered）
    assert!(store.is_recovered());
    // 非 uuid 目录被忽略（防御）
    let junk = dir.path().join("sessions").join("not-a-uuid");
    std::fs::create_dir_all(&junk).unwrap();
    let store3 = Store::open(&cfg).unwrap();
    let r3 = store3.recover().await;
    assert!(!r3.degraded);
}

/// T10-M1 裁决：水位 CRC 损坏 → degraded + WatermarkCorrupt 告警（§17.2）。
#[tokio::test]
async fn t10_watermark_corrupt_degrades() {
    let dir = tempdir().unwrap();
    let cfg = test_config(dir.path().to_path_buf());
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload");
    {
        let store = Store::open(&cfg).unwrap();
        let session = store.create_session(sid).unwrap();
        session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
    }
    // 破坏水位文件
    let wm_path = dir
        .path()
        .join("sessions")
        .join(sid.to_string())
        .join("watermark.json");
    std::fs::write(&wm_path, b"broken").unwrap();
    let store = Store::open(&cfg).unwrap();
    let result = store.recover().await;
    assert!(result.degraded, "watermark corrupt => degraded (M1)");
    assert!(result
        .warnings
        .iter()
        .any(|w| w.code == WarningCode::WatermarkCorrupt));
    // 按无水位处理：以日志尾部为准
    let replay = store.replay_outcome(sid).unwrap();
    assert_eq!(replay.watermark.last_seq, 1);
    assert_eq!(replay.records.len(), 1);
    // Store::status 反映 degraded
    assert!(store.status().degraded);
    assert!(store.status().reason.is_some());
}

/// T10-归档：条件检查（未关闭/未届满/outbox 非全终态 → 不归档；满足 → 移动）。
#[tokio::test]
async fn t10_archive_conditions() {
    let dir = tempdir().unwrap();
    let cfg = test_config(dir.path().to_path_buf());
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload");
    let store = Store::open(&cfg).unwrap();
    let session = store.create_session(sid).unwrap();
    session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
    let now = chrono::Utc::now();
    // 未关闭 → 不归档
    assert!(!store.archive_session(sid, now).unwrap());
    session.mark_closed(now);
    // 保留期未届满 → 不归档
    assert!(!store.archive_session(sid, now).unwrap());
    // 保留期届满（outbox 空 = 全终态真空）→ 归档
    assert!(store
        .archive_session(sid, now + chrono::Duration::days(91))
        .unwrap());
    assert!(store.session(sid).is_none(), "session moved out of sessions map");
    let archived = dir.path().join("archive").join(sid.to_string());
    assert!(archived.is_dir(), "directory moved to archive");
    assert!(!dir.path().join("sessions").join(sid.to_string()).exists());
    // 归档后 session 不存在 → SessionNotFound
    assert!(matches!(
        store.archive_session(sid, now),
        Err(StoreError::SessionNotFound { .. })
    ));
}

/// T10-归档：outbox 非全终态（delivery_unknown 未裁决）→ 不归档（§8.4）。
#[tokio::test]
async fn t10_archive_blocked_by_open_outbox() {
    let dir = tempdir().unwrap();
    let cfg = test_config(dir.path().to_path_buf());
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload");
    let store = Store::open(&cfg).unwrap();
    let session = store.create_session(sid).unwrap();
    session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
    let rec = crate::persist::outbox::NewOutboxRecord {
        command_id: uuid::Uuid::new_v4(),
        session_id: sid,
        command_type: CommandType::Cancel,
        turn_id: None,
        retryable_class: crate::persist::outbox::RetryableClass::NoAutoRedeliver,
    };
    {
        let mut ob = session.outbox().lock().await;
        ob.insert(rec.clone()).unwrap();
        ob.mark_accepted(rec.command_id).unwrap();
        ob.mark_intent_durable(rec.command_id).unwrap();
        ob.mark_dispatched(rec.command_id, chrono::Utc::now()).unwrap();
        ob.mark_delivery_unknown(rec.command_id).unwrap();
    }
    session.mark_closed(chrono::Utc::now());
    let now = chrono::Utc::now() + chrono::Duration::days(91);
    // 未裁决 delivery_unknown 记录 → 不归档
    assert!(!store.archive_session(sid, now).unwrap());
    // 裁决后 → 可归档
    {
        let mut ob = session.outbox().lock().await;
        ob.resolve_delivery_unknown(rec.command_id, DeliveryVerdict::ConfirmedNotDelivered)
            .unwrap();
    }
    assert!(store.archive_session(sid, now).unwrap());
}

/// T11：目录权限 0700 / 文件 0600（unix）。
#[cfg(unix)]
#[tokio::test]
async fn t11_dir_and_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let cfg = test_config(dir.path().to_path_buf());
    let sid = uuid::Uuid::new_v4();
    let d = chat_doc(&sid, b"payload");
    let store = Store::open(&cfg).unwrap();
    assert_eq!(
        std::fs::metadata(cfg.data_dir.join("sessions")).unwrap().permissions().mode() & 0o777,
        0o700,
        "sessions dir 0700"
    );
    assert_eq!(
        std::fs::metadata(cfg.data_dir.join("archive")).unwrap().permissions().mode() & 0o777,
        0o700,
        "archive dir 0700"
    );
    let session = store.create_session(sid).unwrap();
    assert_eq!(
        std::fs::metadata(session.dir()).unwrap().permissions().mode() & 0o777,
        0o700,
        "session dir 0700"
    );
    session.append_update(1, 1, &[(d.0.clone(), &d.1)]).await.unwrap();
    for f in ["updates.log", "watermark.json"] {
        let p = session.dir().join(f);
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600,
            "{f} 0600"
        );
    }
    // outbox 文件 0600
    {
        let mut ob = session.outbox().lock().await;
        let rec = crate::persist::outbox::NewOutboxRecord {
            command_id: uuid::Uuid::new_v4(),
            session_id: sid,
            command_type: CommandType::Create,
            turn_id: None,
            retryable_class: crate::persist::outbox::RetryableClass::SafeToRedeliver,
        };
        ob.insert(rec).unwrap();
    }
    let outbox_path = session.dir().join("outbox.log");
    assert_eq!(
        std::fs::metadata(&outbox_path).unwrap().permissions().mode() & 0o777,
        0o600,
        "outbox.log 0600"
    );
}
