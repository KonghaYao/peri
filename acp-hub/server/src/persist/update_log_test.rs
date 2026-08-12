//! update 日志测试（`docs/plans/f3-persist.md` §11：T1/T2/T3）。
//!
//! T1 blob roundtrip（三类外壳共用原语）；T2 CRC 损坏尾部截断 + corrupt 归档；
//! T3 fsync 语义（per-commit 立即可见；Batch 延迟到 flush）。

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use acp_hub_proto::conn::DocId;
use tempfile::tempdir;

use crate::config::FsyncMode;
use crate::persist::update_log::{
    read_blob, write_blob, BlobReadError, UpdateLog, UpdateLogStats, MAX_RECORD_BYTES,
};
use crate::persist::watermark::WatermarkStore;
use crate::persist::{DegradedFlag, StoreError};

/// 测试辅助：构造 chat 目录 + UpdateLog（PerCommit 默认）。
fn test_log(
    chat_dir: &Path,
    fsync_mode: FsyncMode,
) -> (UpdateLog, Arc<WatermarkStore>, Arc<DegradedFlag>) {
    let chat_id = uuid::Uuid::new_v4();
    std::fs::create_dir_all(chat_dir).unwrap();
    let degraded = Arc::new(DegradedFlag::new());
    let watermark = Arc::new(WatermarkStore::open(chat_dir, fsync_mode, degraded.clone()));
    let log = UpdateLog::open(
        chat_dir,
        chat_id,
        watermark.clone(),
        fsync_mode,
        64 * 1024 * 1024, // 大阈值：单测不触发 compact
        std::time::Duration::from_secs(24 * 3600),
        degraded.clone(),
    )
    .unwrap();
    (log, watermark, degraded)
}

fn chat_doc(chat_id: &uuid::Uuid, payload: &[u8]) -> (DocId, Vec<u8>) {
    (DocId::chat(&chat_id.to_string()), payload.to_vec())
}

#[tokio::test]
async fn t1_blob_roundtrip() {
    // 正常体
    let body = b"hello blob".to_vec();
    let mut buf = Vec::new();
    write_blob(&mut buf, &body).unwrap();
    let mut cur = Cursor::new(&buf);
    let out = read_blob(&mut cur).unwrap().unwrap();
    assert_eq!(out, body);
    // 空体（len=0）合法
    let mut buf2 = Vec::new();
    write_blob(&mut buf2, &[]).unwrap();
    let mut cur2 = Cursor::new(&buf2);
    assert_eq!(read_blob(&mut cur2).unwrap().unwrap(), Vec::<u8>::new());
    // 干净 EOF
    let mut cur3 = Cursor::new(&buf);
    assert_eq!(read_blob(&mut cur3).unwrap(), Some(body.clone()));
    assert_eq!(read_blob(&mut cur3).unwrap(), None);
    // len 越界（> MAX_RECORD）→ Corrupt
    let mut over = Vec::new();
    over.extend_from_slice(&(MAX_RECORD_BYTES + 1).to_le_bytes());
    over.extend_from_slice(&0u32.to_le_bytes());
    let mut cur4 = Cursor::new(&over);
    assert!(matches!(
        read_blob(&mut cur4),
        Err(BlobReadError::Corrupt(_))
    ));
    // CRC 失败 → Corrupt
    let mut bad = buf.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    let mut cur5 = Cursor::new(&bad);
    assert!(matches!(
        read_blob(&mut cur5),
        Err(BlobReadError::Corrupt(_))
    ));
    // 尾部截断（body 半截）→ Corrupt
    let truncated = buf[..buf.len() - 3].to_vec();
    let mut cur6 = Cursor::new(&truncated);
    assert!(matches!(
        read_blob(&mut cur6),
        Err(BlobReadError::Corrupt(_))
    ));
}

#[tokio::test]
async fn t1_update_log_roundtrip_and_replay() {
    let dir = tempdir().unwrap();
    let (mut log, _wm, _deg) = test_log(dir.path(), FsyncMode::PerCommit);
    let sid = uuid::Uuid::new_v4();
    let d1 = chat_doc(&sid, b"update-a");
    let d2 = (
        DocId::session(&sid.to_string()),
        b"update-b".to_vec(),
    );
    log.append(1, 1, &[(d1.0.clone(), &d1.1), (d2.0.clone(), &d2.1)]).await.unwrap();
    log.append(1, 2, &[(d1.0.clone(), &d1.1)]).await.unwrap();
    let stats: UpdateLogStats = log.stats();
    assert_eq!(stats.records, 2);
    assert_eq!(stats.last_seq, 2);
    assert_eq!(stats.last_epoch, 1);
    assert!(stats.bytes > 0);
    // 重新打开（模拟重启）→ 回放全部记录
    drop(log);
    let (mut log2, _wm2, _deg2) = test_log(dir.path(), FsyncMode::PerCommit);
    let outcome = log2.replay().unwrap();
    assert!(!outcome.degraded);
    assert!(outcome.truncated.is_none());
    assert_eq!(outcome.records.len(), 2);
    assert_eq!(outcome.records[0].epoch, 1);
    assert_eq!(outcome.records[0].seq, 1);
    assert_eq!(outcome.records[0].docs.len(), 2);
    assert_eq!(outcome.records[1].seq, 2);
    // epoch 变化
    log2.append(2, 1, &[(d1.0.clone(), &d1.1)]).await.unwrap();
    let stats2 = log2.stats();
    assert_eq!(stats2.last_epoch, 2);
    assert_eq!(stats2.last_seq, 2); // max(1, 2) 语义：新纪元 seq 重置但 last_seq 不回退
}

#[tokio::test]
async fn t2_crc_corruption_truncates_tail() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let d1 = chat_doc(&sid, b"payload-1");
    {
        let (mut log, _wm, _deg) = test_log(dir.path(), FsyncMode::PerCommit);
        log.append(1, 1, &[(d1.0.clone(), &d1.1)]).await.unwrap();
        log.append(1, 2, &[(d1.0.clone(), &d1.1)]).await.unwrap();
        log.append(1, 3, &[(d1.0.clone(), &d1.1)]).await.unwrap();
    }
    // 破坏第 2 条记录的 payload（blob1 = 8 + len1；blob2 payload 起点）
    let path = dir.path().join("updates.log");
    let data = std::fs::read(&path).unwrap();
    let len1 = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let second_payload = 8 + len1 + 8;
    let mut corrupted = data.clone();
    corrupted[second_payload] ^= 0xFF;
    std::fs::write(&path, &corrupted).unwrap();
    // 重开 → 回放 1 条 + TailTruncated + corrupt 段 + degraded
    let (mut log, _wm, deg) = test_log(dir.path(), FsyncMode::PerCommit);
    let outcome = log.replay().unwrap();
    assert_eq!(outcome.records.len(), 1);
    assert_eq!(outcome.records[0].seq, 1);
    assert!(outcome.degraded);
    let t = outcome.truncated.unwrap();
    assert!(t.bytes_kept > 0);
    assert_eq!(t.offset as usize, 8 + len1);
    assert!(deg.is_set());
    // corrupt 段保留
    let corrupt_dir = dir.path().join("corrupt");
    let artifacts: Vec<_> = std::fs::read_dir(&corrupt_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(artifacts.len(), 1);
    assert!(artifacts[0].contains("updates.log"));
    // 文件已截断于损坏点
    let after_len = std::fs::metadata(&path).unwrap().len();
    assert_eq!(after_len as usize, 8 + len1);
    // degraded 语义：拒绝新 committed 承诺（§8.4；新 Action 返回可重试错误）
    let err = log.append(1, 4, &[(d1.0.clone(), &d1.1)]).await.unwrap_err();
    assert!(matches!(err, StoreError::Degraded { .. }), "got {err:?}");
}

#[tokio::test]
async fn t2_corrupt_len_and_version_rejected() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let d1 = chat_doc(&sid, b"x");
    {
        let (mut log, _wm, _deg) = test_log(dir.path(), FsyncMode::PerCommit);
        log.append(1, 1, &[(d1.0.clone(), &d1.1)]).await.unwrap();
    }
    let path = dir.path().join("updates.log");
    // len 字段放大 → 越界判损坏
    let data = std::fs::read(&path).unwrap();
    let mut bad = data.clone();
    bad[0..4].copy_from_slice(&(u32::MAX).to_le_bytes());
    std::fs::write(&path, &bad).unwrap();
    let (mut log, _wm, deg) = test_log(dir.path(), FsyncMode::PerCommit);
    let outcome = log.replay().unwrap();
    assert!(outcome.records.is_empty());
    assert!(outcome.degraded);
    assert!(outcome.truncated.is_some());
    assert!(deg.is_set());
    // 恢复后 version 不符 → 损坏
    std::fs::write(&path, &data).unwrap();
    let (_log2, _wm2, _deg2) = test_log(dir.path(), FsyncMode::PerCommit);
    // 手工构造 version=0x02 的记录
    let mut body = vec![0x02u8, 0x01];
    body.extend_from_slice(&1u32.to_le_bytes());
    body.extend_from_slice(&1u64.to_le_bytes());
    body.extend_from_slice(&[0u8, 1, 0, 0, 0, b'a']); // doc 段
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    write_blob(&mut f, &body).unwrap();
    drop(f);
    let (mut log3, _wm3, deg3) = test_log(dir.path(), FsyncMode::PerCommit);
    let outcome3 = log3.replay().unwrap();
    assert_eq!(outcome3.records.len(), 1); // 旧记录完好
    assert!(outcome3.degraded); // 新记录损坏
    assert!(deg3.is_set());
}

#[tokio::test]
async fn t3_fsync_per_commit_visible_after_reopen() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let d1 = chat_doc(&sid, b"durable");
    {
        let (mut log, _wm, _deg) = test_log(dir.path(), FsyncMode::PerCommit);
        log.append(1, 1, &[(d1.0.clone(), &d1.1)]).await.unwrap();
        // drop 模拟崩溃；PerCommit 已 sync_data
    }
    let (mut log, _wm, _deg) = test_log(dir.path(), FsyncMode::PerCommit);
    let outcome = log.replay().unwrap();
    assert_eq!(outcome.records.len(), 1);
    assert!(outcome.truncated.is_none());
}

#[tokio::test]
async fn t3_batch_mode_defers_flush() {
    let dir = tempdir().unwrap();
    let sid = uuid::Uuid::new_v4();
    let d1 = chat_doc(&sid, b"batched");
    let wm_path;
    {
        let (mut log, wm, _deg) = test_log(dir.path(), FsyncMode::Batch);
        log.append(1, 1, &[(d1.0.clone(), &d1.1)]).await.unwrap();
        // Batch 模式：水位未落盘（仅 dirty），文件系统层未 fsync（语义降级
        // 由上层声明，§8.4）。水位文件不应存在。
        wm_path = wm.path().to_path_buf();
        assert!(!wm_path.exists(), "batch mode must not persist watermark before flush");
        // flush 后水位落盘
        log.flush().unwrap();
        assert!(wm_path.exists());
        assert_eq!(wm.current().last_seq, 1);
    }
    // reopen 可见（同进程 page cache；真实「未 flush 缺失」依赖 OS 崩溃语义，
    // 无法在同进程可靠复现——Ack 降级语义由上层测试）
    let (mut log, _wm, _deg) = test_log(dir.path(), FsyncMode::Batch);
    let outcome = log.replay().unwrap();
    assert_eq!(outcome.records.len(), 1);
}

#[tokio::test]
async fn t_unsupported_doc_id_rejected() {
    let dir = tempdir().unwrap();
    let (mut log, _wm, _deg) = test_log(dir.path(), FsyncMode::PerCommit);
    let registry = (DocId::REGISTRY, b"nope".to_vec());
    let err = log.append(1, 1, &[(registry.0, &registry.1)]).await.unwrap_err();
    assert!(matches!(err, StoreError::Corrupt { .. }), "got {err:?}");
}
