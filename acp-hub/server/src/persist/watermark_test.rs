//! 水位测试（`docs/plans/f3-persist.md` §11：T6 对齐规则 + 损坏处理）。

use std::sync::Arc;

use tempfile::tempdir;

use crate::config::FsyncMode;
use crate::persist::watermark::{AlignmentWarning, Watermark, WatermarkStore};
use crate::persist::DegradedFlag;

/// T6a：对齐规则——缺失 → 日志尾部；epoch 相同取 min（SeqMismatch 告警）；
/// epoch 不同以水位为准（EpochMismatch 告警）。
#[test]
fn t6_align_rules() {
    let dir = tempdir().unwrap();
    let degraded = Arc::new(DegradedFlag::new());
    let wm = WatermarkStore::open(dir.path(), FsyncMode::PerCommit, degraded.clone());

    // 水位缺失 + 日志尾部 → 日志为准
    let (w, warn) = wm.align(None, Some((1, 90)));
    assert_eq!(
        w,
        Watermark {
            epoch: 1,
            last_seq: 90
        }
    );
    assert!(warn.is_none());

    // 无水位无日志 → (0,0)
    let (w, warn) = wm.align(None, None);
    assert_eq!(w, Watermark::default());
    assert!(warn.is_none());

    // epoch 相同且相等 → 无告警
    let (w, warn) = wm.align(
        Some(Watermark {
            epoch: 1,
            last_seq: 90,
        }),
        Some((1, 90)),
    );
    assert_eq!(
        w,
        Watermark {
            epoch: 1,
            last_seq: 90
        }
    );
    assert!(warn.is_none());

    // epoch 相同不等 → min + SeqMismatch（水位 100 vs 日志 90）
    let (w, warn) = wm.align(
        Some(Watermark {
            epoch: 1,
            last_seq: 100,
        }),
        Some((1, 90)),
    );
    assert_eq!(
        w,
        Watermark {
            epoch: 1,
            last_seq: 90
        }
    );
    assert_eq!(
        warn,
        Some(AlignmentWarning::SeqMismatch {
            watermark_seq: 100,
            log_seq: 90
        })
    );
    assert!(!degraded.is_set(), "seq mismatch is warning only");

    // epoch 不同 → 以水位为准 + EpochMismatch
    let (w, warn) = wm.align(
        Some(Watermark {
            epoch: 3,
            last_seq: 40,
        }),
        Some((1, 90)),
    );
    assert_eq!(
        w,
        Watermark {
            epoch: 3,
            last_seq: 40
        }
    );
    assert_eq!(
        warn,
        Some(AlignmentWarning::EpochMismatch {
            watermark_epoch: 3,
            log_epoch: 1
        })
    );

    // 有水位无日志 → 水位为准
    let (w, warn) = wm.align(
        Some(Watermark {
            epoch: 2,
            last_seq: 7,
        }),
        None,
    );
    assert_eq!(
        w,
        Watermark {
            epoch: 2,
            last_seq: 7
        }
    );
    assert!(warn.is_none());

    // 对齐结果写入内存（current 查询）
    assert_eq!(
        wm.current(),
        Watermark {
            epoch: 2,
            last_seq: 7
        }
    );
}

/// T6b：write/load roundtrip；水位损坏（CRC 失败）→ None + degraded（M1 裁决）；
/// 损坏后按无水位处理。
#[test]
fn t6_write_load_roundtrip_and_corruption() {
    let dir = tempdir().unwrap();
    let degraded = Arc::new(DegradedFlag::new());
    let wm = WatermarkStore::open(dir.path(), FsyncMode::PerCommit, degraded.clone());
    // 文件不存在 → None，不 degraded
    assert_eq!(wm.load().unwrap(), None);
    assert!(!degraded.is_set());
    // 写入 → 重新加载 roundtrip
    wm.write(&Watermark {
        epoch: 2,
        last_seq: 55,
    })
    .unwrap();
    let wm2 = WatermarkStore::open(dir.path(), FsyncMode::PerCommit, degraded.clone());
    assert_eq!(
        wm2.load().unwrap(),
        Some(Watermark {
            epoch: 2,
            last_seq: 55
        })
    );
    // 覆盖写
    wm.write(&Watermark {
        epoch: 2,
        last_seq: 60,
    })
    .unwrap();
    let wm3 = WatermarkStore::open(dir.path(), FsyncMode::PerCommit, degraded.clone());
    assert_eq!(
        wm3.load().unwrap(),
        Some(Watermark {
            epoch: 2,
            last_seq: 60
        })
    );
    // 损坏（写入垃圾字节）→ None + degraded（§17.2，M1 裁决不降级到告警）
    let path = wm.path();
    std::fs::write(path, b"garbage not a blob").unwrap();
    let wm4 = WatermarkStore::open(dir.path(), FsyncMode::PerCommit, degraded.clone());
    let loaded = wm4.load().unwrap();
    assert_eq!(loaded, None, "corrupt watermark treated as absent");
    assert!(degraded.is_set(), "watermark corrupt => degraded (M1)");
    // 损坏后按无水位处理：对齐以日志尾部为准
    let (w, warn) = wm4.align(None, Some((1, 30)));
    assert_eq!(
        w,
        Watermark {
            epoch: 1,
            last_seq: 30
        }
    );
    assert!(warn.is_none());
}

/// T6c：Batch 模式 write 延迟落盘，flush 后可见。
#[test]
fn t6_batch_mode_deferred_persist() {
    let dir = tempdir().unwrap();
    let degraded = Arc::new(DegradedFlag::new());
    let wm = WatermarkStore::open(dir.path(), FsyncMode::Batch, degraded.clone());
    wm.write(&Watermark {
        epoch: 1,
        last_seq: 9,
    })
    .unwrap();
    assert_eq!(wm.current().last_seq, 9);
    assert!(
        !wm.path().exists(),
        "batch write must not hit disk before flush"
    );
    wm.flush().unwrap();
    assert!(wm.path().exists());
    let wm2 = WatermarkStore::open(dir.path(), FsyncMode::PerCommit, degraded.clone());
    assert_eq!(
        wm2.load().unwrap(),
        Some(Watermark {
            epoch: 1,
            last_seq: 9
        })
    );
}
