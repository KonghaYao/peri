//! (epoch, last_seq) 水位（§4.5.1/§6/§8.5）：补推起点。
//!
//! 每 chat 独立小文件 `watermark.json`（§2 目录布局），单条 blob + JSON
//! 包裹（§4.1）。更新时机：每次 `UpdateLog::append` 成功后更新（epoch 相同只
//! 推进 seq；epoch 变化则替换）——append 顺序 = 写日志记录 → fsync → 写水位
//! → fsync → 返回（§4.3）。崩溃于两者之间 → 水位落后 → 对齐规则（[`align`]）
//! 吸收。
//!
//! 加载与对齐规则（§8.4.1 不变量 2）：
//! 1. 水位缺失（新 chat/文件损坏）→ 以日志尾部为准；无日志 → `(0, 0)`
//!    （从 1 开始补推，instance 环形滑窗 500 条兜底，§8.5）；
//! 2. 水位与日志尾部 epoch 相同：`last_seq = min(水位, 日志)`，不等 →
//!    `SeqMismatch` 告警；
//! 3. epoch 不同：旧流 seq 空间作废（§4.5.1），以水位为准（水位为权威代际），
//!    `EpochMismatch` 告警；是否判不可校准 gap 属上层（chat 状态机）裁决。

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing::warn;

use crate::config::FsyncMode;

use crate::persist::update_log::{read_blob, write_blob, BlobReadError, BLOB_VERSION};
use crate::persist::{DegradedFlag, StoreError};

/// 水位文件名（§2 目录布局；「.json」仅表意，实际为 blob 外壳）。
pub const WATERMARK_FILE: &str = "watermark.json";

/// 水位 JSON 形态（单条 blob 包裹）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WatermarkFile {
    v: u8,
    epoch: u32,
    last_seq: u64,
    updated_at: chrono::DateTime<chrono::Utc>,
}

/// (epoch, last_seq) 水位（§4.5.1/§6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Watermark {
    /// 流纪元（instance 侧 per-chat 代际，§4.5.1）。
    pub epoch: u32,
    /// 已落盘的最大帧 seq（§8.5：`from_seq = last_seq + 1`）。
    pub last_seq: u64,
}

/// 水位对齐告警（§6 加载与对齐规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignmentWarning {
    /// 水位与日志尾部 seq 不一致（以较小者为准；日志尾部截断 seq 倒退场景）。
    SeqMismatch {
        /// 水位 last_seq。
        watermark_seq: u64,
        /// 日志尾部 last_seq。
        log_seq: u64,
    },
    /// 水位 epoch 与日志尾部不一致（以水位为准，旧流 seq 空间作废）。
    EpochMismatch {
        /// 水位 epoch。
        watermark_epoch: u32,
        /// 日志尾部 epoch。
        log_epoch: u32,
    },
}

/// 水位存储（每 chat 独立小文件，§6）。
///
/// 内部 `Mutex` 保护内存值（对齐结果）与 dirty 标记；文件 I/O 为同步小操作
/// （设计稿 §10 并发模型）。写采用 tmp → fsync → rename → 目录 fsync（与
/// auth persist 同纪律），崩溃不产生半文件。
pub struct WatermarkStore {
    path: PathBuf,
    tmp_path: PathBuf,
    fsync_mode: FsyncMode,
    degraded: std::sync::Arc<DegradedFlag>,
    state: Mutex<WatermarkState>,
}

struct WatermarkState {
    current: Watermark,
    /// Batch 模式下文件未落盘标记（PerCommit 始终落盘）。
    dirty: bool,
}

impl WatermarkStore {
    /// 打开（或创建）chat 的水位存储。
    pub fn open(
        chat_dir: &Path,
        fsync_mode: FsyncMode,
        degraded: std::sync::Arc<DegradedFlag>,
    ) -> Self {
        WatermarkStore {
            path: chat_dir.join(WATERMARK_FILE),
            tmp_path: chat_dir.join(format!("{WATERMARK_FILE}.tmp")),
            fsync_mode,
            degraded,
            state: Mutex::new(WatermarkState {
                current: Watermark::default(),
                dirty: false,
            }),
        }
    }

    /// 加载水位文件（§6）：单条 blob + JSON。损坏（CRC 失败/解析失败/version
    /// 不符）→ 告警 + degraded + `Ok(None)`（视为无水位，M1 裁决：不降级到
    /// 告警）。文件不存在 → `Ok(None)`。
    pub fn load(&self) -> Result<Option<Watermark>, StoreError> {
        let path = self.path.clone();
        let mut f = match fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(StoreError::Io {
                    path: path.clone(),
                    source: e,
                })
            }
        };
        match read_blob(&mut f) {
            Ok(Some(body)) => match serde_json::from_slice::<WatermarkFile>(&body) {
                Ok(wf) => {
                    if wf.v != BLOB_VERSION {
                        self.mark_corrupt("watermark version mismatch");
                        return Ok(None);
                    }
                    let wm = Watermark {
                        epoch: wf.epoch,
                        last_seq: wf.last_seq,
                    };
                    self.state.lock().expect("watermark lock poisoned").current = wm;
                    Ok(Some(wm))
                }
                Err(e) => {
                    self.mark_corrupt(&format!("watermark parse failed: {e}"));
                    Ok(None)
                }
            },
            Ok(None) => {
                self.mark_corrupt("watermark file empty");
                Ok(None)
            }
            Err(BlobReadError::Corrupt(detail)) => {
                self.mark_corrupt(&detail);
                Ok(None)
            }
            Err(BlobReadError::Io(e)) => Err(StoreError::Io {
                path: path.clone(),
                source: e,
            }),
        }
    }

    /// 损坏水位：告警 + degraded（§17.2：启动恢复不变量失败 → Degraded，
    /// 主管 M1 裁决保持）。
    fn mark_corrupt(&self, detail: &str) {
        self.degraded.set(format!("watermark corrupt: {detail}"));
        warn!(
            path = %self.path.display(), reason = detail,
            "watermark corrupt; treated as absent, store degraded"
        );
    }

    /// 写水位（覆盖写 + fsync，§6）。PerCommit 立即落盘；Batch 模式标记
    /// dirty，由 [`WatermarkStore::flush`] 统一落盘（Ack 降级语义由上层声明）。
    pub fn write(&self, wm: &Watermark) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("watermark lock poisoned");
        state.current = *wm;
        if self.fsync_mode == FsyncMode::Batch {
            state.dirty = true;
            return Ok(());
        }
        drop(state);
        self.persist()
    }

    /// Batch 模式统一落盘（§4.3/§16）。
    pub fn flush(&self) -> Result<(), StoreError> {
        let mut state = self.state.lock().expect("watermark lock poisoned");
        if !state.dirty {
            return Ok(());
        }
        state.dirty = false;
        drop(state);
        self.persist()
    }

    /// 实际写文件：tmp → fsync → rename → 目录 fsync。
    fn persist(&self) -> Result<(), StoreError> {
        let wm = self.state.lock().expect("watermark lock poisoned").current;
        let body = serde_json::to_vec(&WatermarkFile {
            v: BLOB_VERSION,
            epoch: wm.epoch,
            last_seq: wm.last_seq,
            updated_at: chrono::Utc::now(),
        })
        .map_err(|e| StoreError::Corrupt {
            path: self.path.clone(),
            detail: format!("watermark serialize failed: {e}"),
        })?;
        let tmp_path = self.tmp_path.clone();
        let path = self.path.clone();
        let mut tmp = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            })?;
        if let Err(e) = write_blob(&mut tmp, &body) {
            self.fail(&format!("watermark tmp write failed: {e}"));
            return Err(StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            });
        }
        if let Err(e) = tmp.sync_all() {
            self.fail(&format!("watermark tmp fsync failed: {e}"));
            return Err(StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            });
        }
        drop(tmp);
        if let Err(e) = fs::rename(&tmp_path, &path) {
            self.fail(&format!("watermark rename failed: {e}"));
            return Err(StoreError::Io {
                path: path.clone(),
                source: e,
            });
        }
        // 文件权限 0600（§8.4/§9.1）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
                self.fail(&format!("watermark chmod failed: {e}"));
                return Err(StoreError::Io {
                    path: path.clone(),
                    source: e,
                });
            }
        }
        crate::persist::update_log::sync_dir(self.path.parent().expect("chat dir"))?;
        Ok(())
    }

    /// 落盘失败：degraded + 告警（绝不静默，§8.4）。
    fn fail(&self, detail: &str) {
        self.degraded
            .set(format!("watermark write failed: {detail}"));
        warn!(
            path = %self.path.display(), reason = detail,
            "watermark write failed; store degraded"
        );
    }

    /// 对齐（§6/§8.4.1 不变量 2）：与日志最后一条 (epoch, seq) 核对。
    ///
    /// 规则：水位缺失 → 日志尾部为准（无日志 → `(0,0)`）；epoch 相同 →
    /// `min(水位, 日志)`（不等 → `SeqMismatch` 告警）；epoch 不同 → 水位为准
    /// （`EpochMismatch` 告警）。对齐结果写入内存（`current()` 查询）。
    pub fn align(
        &self,
        wm: Option<Watermark>,
        log_tail: Option<(u32, u64)>,
    ) -> (Watermark, Option<AlignmentWarning>) {
        let (aligned, warning) = match (wm, log_tail) {
            (None, Some((le, ls))) => (
                Watermark {
                    epoch: le,
                    last_seq: ls,
                },
                None,
            ),
            (None, None) => (Watermark::default(), None),
            (Some(w), None) => (w, None),
            (Some(w), Some((le, ls))) => {
                if w.epoch == le {
                    if w.last_seq == ls {
                        (w, None)
                    } else {
                        let seq = w.last_seq.min(ls);
                        (
                            Watermark {
                                epoch: le,
                                last_seq: seq,
                            },
                            Some(AlignmentWarning::SeqMismatch {
                                watermark_seq: w.last_seq,
                                log_seq: ls,
                            }),
                        )
                    }
                } else {
                    (
                        w,
                        Some(AlignmentWarning::EpochMismatch {
                            watermark_epoch: w.epoch,
                            log_epoch: le,
                        }),
                    )
                }
            }
        };
        self.state.lock().expect("watermark lock poisoned").current = aligned;
        (aligned, warning)
    }

    /// 当前水位（对齐结果；补推起点查询，§6）。
    pub fn current(&self) -> Watermark {
        self.state.lock().expect("watermark lock poisoned").current
    }

    /// degraded 状态（供 Store 聚合，§7）。
    pub fn degraded_is_set(&self) -> bool {
        self.degraded.is_set()
    }

    /// 水位文件路径（诊断/测试）。
    pub fn path(&self) -> &Path {
        &self.path
    }
}
