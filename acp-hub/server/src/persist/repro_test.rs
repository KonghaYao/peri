//! 审查复现测试：验证疑点（临时验证 crate，不进入仓库）。

use std::sync::Arc;

use acp_hub_proto::conn::DocId;
use tempfile::tempdir;

use crate::config::FsyncMode;
use crate::persist::outbox::{CommandType, NewOutboxRecord, OutboxStore, RetryableClass};
use crate::persist::update_log::UpdateLog;
use crate::persist::watermark::WatermarkStore;
use crate::persist::{DegradedFlag, StoreError};

fn chat_doc(chat_id: &uuid::Uuid, payload: &[u8]) -> (DocId, Vec<u8>) {
    (DocId::chat(&chat_id.to_string()), payload.to_vec())
}

/// 疑点 1（P1）：快照存在 + 日志首条记录结构损坏 → 恢复编排必须报告
/// TailTruncated + degraded + 保留 corrupt 段；修复前 probe 计数提前停止导致
/// 整日志静默清空（无信号）。
#[tokio::test]
async fn repro1_mid_log_corruption_silently_truncates_newer_records() {
    use crate::persist::store::Store;
    use crate::persist::store::CHATS_DIR;
    use crate::persist::PersistConfig;

    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let d1 = chat_doc(&sid, b"payload");

    // 阶段 1：seed chat（append 1..=3 + compact 快照点 3 + append 4..=6）
    {
        let cfg = PersistConfig {
            data_dir: dir.path().to_path_buf(),
            fsync_mode: FsyncMode::PerCommit,
            compact_threshold_bytes: 64 * 1024 * 1024,
            compact_interval: std::time::Duration::from_secs(24 * 3600),
            disk_budget: 2 * 1024 * 1024 * 1024,
            outbox_retention: std::time::Duration::from_secs(7 * 86_400),
            archive_retention: std::time::Duration::from_secs(90 * 86_400),
        };
        let store = Store::open(&cfg).unwrap();
        let chat = store.create_chat(sid).unwrap();
        let mut log = chat.update_log().lock().await;
        for seq in 1..=3u64 {
            log.append(1, seq, &[(d1.0.clone(), &d1.1)]).await.unwrap();
        }
        log.compact(
            [(d1.0.clone(), b"full-snapshot".to_vec())]
                .into_iter()
                .collect(),
        )
        .await
        .unwrap();
        for seq in 4..=6u64 {
            log.append(1, seq, &[(d1.0.clone(), &d1.1)]).await.unwrap();
        }
        drop(log);
    }
    // 阶段 2：破坏日志首条记录的 len 字段（结构损坏）
    let path = dir
        .path()
        .join(CHATS_DIR)
        .join(sid.to_string())
        .join("updates.log");
    let data = std::fs::read(&path).unwrap();
    let mut corrupted = data.clone();
    corrupted[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(&path, &corrupted).unwrap();

    // 阶段 3：recover —— 期望损坏被报告（TailTruncated 告警 + degraded +
    // corrupt 段保留），而非静默清空
    let cfg = PersistConfig {
        data_dir: dir.path().to_path_buf(),
        fsync_mode: FsyncMode::PerCommit,
        compact_threshold_bytes: 64 * 1024 * 1024,
        compact_interval: std::time::Duration::from_secs(24 * 3600),
        disk_budget: 2 * 1024 * 1024 * 1024,
        outbox_retention: std::time::Duration::from_secs(7 * 86_400),
        archive_retention: std::time::Duration::from_secs(90 * 86_400),
    };
    let store = Store::open(&cfg).unwrap();
    let result = store.recover().await;
    let has_tail = result
        .warnings
        .iter()
        .any(|w| w.code == crate::persist::WarningCode::TailTruncated);
    assert!(
        has_tail && result.degraded && !result.corrupt_artifacts.is_empty(),
        "BUG: corruption not reported: {result:?}"
    );
    // 快照点 3 之上的记录 4..=6 按尾部截断语义丢弃，但损坏必须可见
    let corrupt_dir = dir
        .path()
        .join(CHATS_DIR)
        .join(sid.to_string())
        .join("corrupt");
    let n = std::fs::read_dir(&corrupt_dir)
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert!(n >= 1, "corrupt segment should be preserved (got {n})");
    // 且日志应截断于损坏点（而非整日志清空后空文件）——日志保留损坏点前的
    // 完好记录（此处损坏点在 offset 0，日志应为空但信号存在）
}

/// 疑点 3（P2）：UpdateLog::replay 后 records 双计数（probe_tail 计数 +
/// replay 累加），stats().records 翻倍。
#[tokio::test]
async fn repro3_replay_double_counts_records() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let d1 = chat_doc(&sid, b"payload");
    let degraded = Arc::new(DegradedFlag::new());
    let wm = Arc::new(WatermarkStore::open(
        dir.path(),
        FsyncMode::PerCommit,
        degraded.clone(),
    ));
    let mut log = UpdateLog::open(
        dir.path(),
        sid,
        wm.clone(),
        FsyncMode::PerCommit,
        64 * 1024 * 1024,
        std::time::Duration::from_secs(24 * 3600),
        degraded.clone(),
    )
    .unwrap();
    for seq in 1..=3u64 {
        log.append(1, seq, &[(d1.0.clone(), &d1.1)]).await.unwrap();
    }
    assert_eq!(
        log.stats().records,
        3,
        "precondition: 3 records after append"
    );
    // 重新打开（probe_tail 计数 3）→ replay 再 +3 → 6
    drop(log);
    let degraded2 = Arc::new(DegradedFlag::new());
    let wm2 = Arc::new(WatermarkStore::open(
        dir.path(),
        FsyncMode::PerCommit,
        degraded2.clone(),
    ));
    let mut log2 = UpdateLog::open(
        dir.path(),
        sid,
        wm2.clone(),
        FsyncMode::PerCommit,
        64 * 1024 * 1024,
        std::time::Duration::from_secs(24 * 3600),
        degraded2.clone(),
    )
    .unwrap();
    let outcome = log2.replay().unwrap();
    assert_eq!(outcome.records.len(), 3);
    assert_eq!(
        log2.stats().records,
        3,
        "BUG CONFIRMED: stats.records after replay = {} (expected 3)",
        log2.stats().records
    );
}

/// 疑点 5b（P2）：outbox 物理压缩后 outbox.log 权限不是 0600（tmp 文件
/// 未 set_permissions）。
#[cfg(unix)]
#[test]
fn repro5b_outbox_compact_permissions_not_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let degraded = Arc::new(DegradedFlag::new());
    let mut ob = OutboxStore::open(
        dir.path(),
        FsyncMode::PerCommit,
        std::time::Duration::from_secs(7 * 86_400),
        degraded.clone(),
    )
    .unwrap();
    let sid = uuid::Uuid::new_v4();
    let cid = uuid::Uuid::new_v4();
    ob.insert(NewOutboxRecord {
        command_id: cid,
        chat_id: sid,
        command_type: CommandType::Create,
        turn_id: None,
        retryable_class: RetryableClass::SafeToRedeliver,
    })
    .unwrap();
    ob.mark_accepted(cid).unwrap();
    ob.mark_intent_durable(cid).unwrap();
    ob.mark_dispatched(cid, chrono::Utc::now()).unwrap();
    ob.mark_delivery_confirmed(cid).unwrap();
    ob.mark_projection_committed(cid).unwrap();
    ob.mark_completed(cid).unwrap();
    // 到期清理触发压缩：updated_at 改不了，用短 retention 重开
    drop(ob);
    let mut ob2 = OutboxStore::open(
        dir.path(),
        FsyncMode::PerCommit,
        std::time::Duration::from_secs(1),
        degraded,
    )
    .unwrap();
    ob2.replay_from_disk().unwrap();
    // 等 1.2s 让 updated_at 超过 1s retention
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let stats = ob2.cleanup(chrono::Utc::now(), true);
    assert!(
        stats.removed >= 1,
        "precondition: cleanup removed records (stats={stats:?})"
    );
    assert!(stats.compressed, "precondition: compaction happened");
    let mode = std::fs::metadata(dir.path().join("outbox.log"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "outbox.log permissions should be 0600 after compaction (got {mode:o})"
    );
}

/// 疑点 5c（P2）：corrupt/ 段文件权限非 0600（fs::write 默认 umask 0644，
/// 段含 yjs 字节）。
#[cfg(unix)]
#[tokio::test]
async fn repro5c_corrupt_segment_permissions_not_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let d1 = chat_doc(&sid, b"payload");
    let degraded = Arc::new(DegradedFlag::new());
    let wm = Arc::new(WatermarkStore::open(
        dir.path(),
        FsyncMode::PerCommit,
        degraded.clone(),
    ));
    let mut log = UpdateLog::open(
        dir.path(),
        sid,
        wm.clone(),
        FsyncMode::PerCommit,
        64 * 1024 * 1024,
        std::time::Duration::from_secs(24 * 3600),
        degraded.clone(),
    )
    .unwrap();
    log.append(1, 1, &[(d1.0.clone(), &d1.1)]).await.unwrap();
    log.append(1, 2, &[(d1.0.clone(), &d1.1)]).await.unwrap();
    drop(log);
    // 破坏第 2 条 payload
    let path = dir.path().join("updates.log");
    let data = std::fs::read(&path).unwrap();
    let len1 = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let mut corrupted = data.clone();
    corrupted[8 + len1 + 8] ^= 0xFF;
    std::fs::write(&path, &corrupted).unwrap();
    let degraded2 = Arc::new(DegradedFlag::new());
    let wm2 = Arc::new(WatermarkStore::open(
        dir.path(),
        FsyncMode::PerCommit,
        degraded2.clone(),
    ));
    let mut log2 = UpdateLog::open(
        dir.path(),
        sid,
        wm2.clone(),
        FsyncMode::PerCommit,
        64 * 1024 * 1024,
        std::time::Duration::from_secs(24 * 3600),
        degraded2.clone(),
    )
    .unwrap();
    let outcome = log2.replay().unwrap();
    assert!(outcome.truncated.is_some());
    let artifacts: Vec<_> = std::fs::read_dir(dir.path().join("corrupt"))
        .unwrap()
        .flatten()
        .collect();
    assert!(!artifacts.is_empty());
    let mode = artifacts[0].metadata().unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "corrupt segment permissions should be 0600 (got {mode:o})"
    );
}

/// 疑点 5（P2）：compact 后 updates.snapshot 文件权限不是 0600（tmp 文件
/// 未 set_permissions，rename 后继承 0644 & umask）。
#[cfg(unix)]
#[tokio::test]
async fn repro5_snapshot_permissions_not_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let d1 = chat_doc(&sid, b"payload");
    let degraded = Arc::new(DegradedFlag::new());
    let wm = Arc::new(WatermarkStore::open(
        dir.path(),
        FsyncMode::PerCommit,
        degraded.clone(),
    ));
    let mut log = UpdateLog::open(
        dir.path(),
        sid,
        wm.clone(),
        FsyncMode::PerCommit,
        64 * 1024 * 1024,
        std::time::Duration::from_secs(24 * 3600),
        degraded.clone(),
    )
    .unwrap();
    log.append(1, 1, &[(d1.0.clone(), &d1.1)]).await.unwrap();
    log.compact(
        [(d1.0.clone(), b"secret-doc-content".to_vec())]
            .into_iter()
            .collect(),
    )
    .await
    .unwrap();
    let mode = std::fs::metadata(dir.path().join("updates.snapshot"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "snapshot permissions should be 0600 (got {mode:o})"
    );
}

/// 疑点 6（P2）：mark_failed 注释声称 projection_committed → failed 合法，
/// 但 allowed_transition 表缺该对 → 实际返回 InvalidTransition，记录无法
/// 终态化（卡在 projection_committed，清理/归档前置条件「全终态」永不满足）。
#[test]
fn repro6_projection_committed_to_failed_rejected() {
    let dir = tempdir().unwrap();
    let degraded = Arc::new(DegradedFlag::new());
    let mut outbox = OutboxStore::open(
        dir.path(),
        FsyncMode::PerCommit,
        std::time::Duration::from_secs(7 * 86_400),
        degraded,
    )
    .unwrap();
    let sid = uuid::Uuid::new_v4();
    let cid = uuid::Uuid::new_v4();
    outbox
        .insert(NewOutboxRecord {
            command_id: cid,
            chat_id: sid,
            command_type: CommandType::Create,
            turn_id: None,
            retryable_class: RetryableClass::SafeToRedeliver,
        })
        .unwrap();
    outbox.mark_accepted(cid).unwrap();
    outbox.mark_intent_durable(cid).unwrap();
    outbox.mark_dispatched(cid, chrono::Utc::now()).unwrap();
    outbox.mark_delivery_confirmed(cid).unwrap();
    outbox.mark_projection_committed(cid).unwrap();
    // 投影落盘后业务失败（action_error，非 retryable）
    let err = outbox.mark_failed(
        cid,
        crate::persist::outbox::LastError {
            code: "INVALID_STATE".into(),
            retryable: false,
            at: chrono::Utc::now(),
        },
    );
    assert!(
        !matches!(err, Err(StoreError::InvalidTransition { .. })),
        "BUG CONFIRMED: projection_committed -> failed rejected: {err:?}"
    );
}
