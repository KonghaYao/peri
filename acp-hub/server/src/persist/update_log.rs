//! update 日志（§4）：blob 线格式原语 + UpdateLog。
//!
//! blob 外壳（§4.1，三种持久化文件共用）：
//!
//! ```text
//! len: u32 LE —— 记录体字节数（不含本字段与 crc32）
//! crc32: u32 LE —— CRC32(记录体)
//! body —— 记录体（len 字节）
//! ```
//!
//! - CRC 覆盖整个记录体（含 version/kind 头）；len 被改小 → 读到错位体 → CRC
//!   失败；len 被改大 → EOF/越界 → 判损坏。
//! - body 首字节 `version: u8`（当前 0x01），版本不符 = 损坏。
//! - 防御上限 [`MAX_RECORD_BYTES`]（64MB，§4.1）：越界视为损坏。
//!
//! 逻辑提交记录（§4.2，一条记录 = 一个逻辑提交）：
//!
//! ```text
//! body = version:u8 | kind:u8 | epoch:u32 LE | seq:u64 LE | payload
//! kind = 0x01 doc_commit（M1 唯一）
//! doc_commit payload = 重复段：doc_id:u8（0=chat,1=session）| len:u32 LE | yjs update 字节
//! ```
//!
//! 并发：写锁（`tokio::sync::Mutex`，由 [`crate::persist::ChatStore`] 持有）
//! 串行化 append 与 compact（§4.3/§8）。fsync 纪律（§8.4）：PerCommit 模式
//! `sync_data()` per append；Batch 模式延迟到 [`UpdateLog::flush`]。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek as _, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use acp_hub_proto::conn::DocId;
use tracing::{debug, warn};

use crate::config::FsyncMode;

use crate::persist::watermark::{Watermark, WatermarkStore};
use crate::persist::{DegradedFlag, StoreError};

/// blob body 版本号（§4.1，当前 0x01；不符 = 损坏）。
pub(crate) const BLOB_VERSION: u8 = 0x01;

/// 记录 kind：doc_commit（§4.2，M1 唯一，预留扩展）。
const KIND_DOC_COMMIT: u8 = 0x01;

/// 防御上限（§4.1）：单帧上限 1MB（§16），一个微批次（§6.4）+ 双 Doc 段的
/// 逻辑提交远小于此；越界视为损坏。
pub(crate) const MAX_RECORD_BYTES: u32 = 64 * 1024 * 1024;

/// update 日志文件名（§2 目录布局）。
pub const UPDATES_LOG_FILE: &str = "updates.log";
/// compact 快照文件名（§2 目录布局）。
pub const UPDATES_SNAPSHOT_FILE: &str = "updates.snapshot";
/// compact 临时快照文件名（原子流程中间态，§8）。
pub const UPDATES_SNAPSHOT_TMP_FILE: &str = "updates.snapshot.tmp";
/// corrupt 目录名（§2 目录布局）。
pub const CORRUPT_DIR: &str = "corrupt";

/// blob 读取错误：区分干净 EOF（正常结束）与损坏。
#[derive(Debug)]
pub(crate) enum BlobReadError {
    /// I/O 失败。
    Io(io::Error),
    /// 记录损坏（CRC/越界/结构非法/version 不符；含尾部截断）。
    Corrupt(String),
}

impl From<io::Error> for BlobReadError {
    fn from(e: io::Error) -> Self {
        BlobReadError::Io(e)
    }
}

/// 写一条 blob 记录（len + crc32 + body）。CRC32 覆盖记录体。
pub(crate) fn write_blob(w: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "blob body too large"))?;
    let crc = crc32fast::hash(body);
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&crc.to_le_bytes())?;
    w.write_all(body)
}

/// 读一条 blob 记录（§4.1 读流程）：
/// `read_exact(8)` → 校验 len ≤ MAX → `read_exact(len)` → CRC 校验。
///
/// 返回 `None` = 头部 EOF（干净文件尾）。头部半截 / body 半截 / len 越界 /
/// CRC 失败 → `Corrupt`。
pub(crate) fn read_blob(r: &mut impl Read) -> Result<Option<Vec<u8>>, BlobReadError> {
    let mut header = [0u8; 8];
    match r.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(BlobReadError::Io(e)),
    }
    let len = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
    let crc = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes"));
    if len > MAX_RECORD_BYTES {
        return Err(BlobReadError::Corrupt(format!(
            "record len {len} exceeds MAX_RECORD_BYTES"
        )));
    }
    let mut body = vec![0u8; len as usize];
    if let Err(e) = r.read_exact(&mut body) {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return Err(BlobReadError::Corrupt("truncated record body".into()));
        }
        return Err(BlobReadError::Io(e));
    }
    if crc32fast::hash(&body) != crc {
        return Err(BlobReadError::Corrupt("crc32 mismatch".into()));
    }
    Ok(Some(body))
}

/// 编码 doc_commit 记录体（§4.2）。
fn encode_doc_commit(epoch: u32, seq: u64, docs: &[(DocId, &[u8])]) -> Result<Vec<u8>, StoreError> {
    let mut body = Vec::with_capacity(8 + 9 * docs.len());
    body.push(BLOB_VERSION);
    body.push(KIND_DOC_COMMIT);
    body.extend_from_slice(&epoch.to_le_bytes());
    body.extend_from_slice(&seq.to_le_bytes());
    for (doc, update) in docs {
        let prefix = doc.as_str();
        let id = match prefix.strip_prefix("chat:") {
            Some(_) => 0u8,
            None => match prefix.strip_prefix("session:") {
                Some(_) => 1u8,
                None => {
                    return Err(StoreError::Corrupt {
                        path: PathBuf::from("updates.log"),
                        detail: format!("unsupported doc id {prefix:?} in update log"),
                    })
                }
            },
        };
        let len = u32::try_from(update.len())
            .map_err(|_| StoreError::Corrupt {
                path: PathBuf::from("updates.log"),
                detail: "doc update exceeds u32 len".into(),
            })?;
        body.push(id);
        body.extend_from_slice(&len.to_le_bytes());
        body.extend_from_slice(update);
    }
    Ok(body)
}

/// 解码后的 doc_commit 记录（epoch, seq, docs）。
pub(crate) type DecodedCommit = (u32, u64, Vec<(DocId, Vec<u8>)>);

/// 解码 doc_commit 记录体；返回 (epoch, seq, docs)。结构非法 → Err。
fn decode_doc_commit(chat_id: &uuid::Uuid, body: &[u8]) -> Result<DecodedCommit, String> {
    if body.len() < 14 {
        return Err("record body too short".into());
    }
    let version = body[0];
    if version != BLOB_VERSION {
        return Err(format!("unsupported blob version {version:#04x}"));
    }
    let kind = body[1];
    if kind != KIND_DOC_COMMIT {
        return Err(format!("unsupported record kind {kind:#04x}"));
    }
    let epoch = u32::from_le_bytes(body[2..6].try_into().expect("4 bytes"));
    let seq = u64::from_le_bytes(body[6..14].try_into().expect("8 bytes"));
    let mut docs = Vec::new();
    let mut rest = &body[14..];
    while !rest.is_empty() {
        let id = rest[0];
        if rest.len() < 5 {
            return Err("truncated doc segment header".into());
        }
        let len = u32::from_le_bytes(rest[1..5].try_into().expect("4 bytes"));
        let payload = rest
            .get(5..5 + len as usize)
            .ok_or_else(|| "doc segment len out of bounds".to_string())?;
        let doc = match id {
            0 => DocId::chat(&chat_id.to_string()),
            1 => DocId::session(&chat_id.to_string()),
            other => return Err(format!("unsupported doc id byte {other}")),
        };
        docs.push((doc, payload.to_vec()));
        rest = &rest[5 + len as usize..];
    }
    Ok((epoch, seq, docs))
}

/// 单条日志记录（§4.4 回放产物，交由 doc-manager 按序应用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// 流纪元（§4.5.1）。
    pub epoch: u32,
    /// 本提交覆盖的最大帧 seq（§8.5）。
    pub seq: u64,
    /// 双 Doc 段（chat/control）。
    pub docs: Vec<(DocId, Vec<u8>)>,
}

/// 损坏点信息（§4.4 尾部截断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorruptionInfo {
    /// 损坏记录的文件偏移（blob 起点）。
    pub offset: u64,
    /// 损坏点至 EOF 的字节数（写入 corrupt/ 的段长）。
    pub bytes_kept: u64,
    /// 损坏原因（脱敏）。
    pub reason: String,
}

/// update 日志回放结果（§4.4）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayOutcome {
    /// 按追加序的全部完好记录（快照基线之上的增量由 doc-manager 幂等跳过）。
    pub records: Vec<LogRecord>,
    /// 尾部截断信息；`None` = 日志完整。
    pub truncated: Option<CorruptionInfo>,
    /// 日志损坏 → degraded（§8.4）。
    pub degraded: bool,
    /// 回放期防御性告警（§4.4：同 epoch 内 seq 非单调等；不阻塞，供
    /// recover 编排聚合进 [`crate::persist::RecoveryResult::warnings`]）。
    pub warnings: Vec<crate::persist::RecoveryWarning>,
}

/// compact 全量快照（§8 原子流程产物，单条 blob + JSON）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// 快照点的流纪元。
    pub last_epoch: u32,
    /// 快照点的最大帧 seq（边界，§8）。
    pub last_applied_seq: u64,
    /// 双 Doc 全量 state update（chat/control）。
    pub docs: std::collections::HashMap<DocId, Vec<u8>>,
}

/// 快照文件 JSON 形态（单条 blob 包裹，§8 步骤 2）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotFile {
    v: u8,
    last_epoch: u32,
    last_applied_seq: u64,
    created_at: chrono::DateTime<chrono::Utc>,
    docs: std::collections::HashMap<DocId, Vec<u8>>,
}

/// update 日志统计（§17.1 指标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateLogStats {
    /// 日志文件当前字节数。
    pub bytes: u64,
    /// 日志记录条数。
    pub records: u64,
    /// 最后一条记录的 seq（无记录 = 0）。
    pub last_seq: u64,
    /// 最后一条记录的 epoch。
    pub last_epoch: u32,
}

/// update 日志（§4）：追加 blob 记录 + 启动回放（尾部截断）+ compact + 快照。
///
/// 状态（文件句柄/计数）由外层 `tokio::sync::Mutex` 串行化（设计稿 §10
/// 并发模型）；`&mut self` 方法必须在持锁后调用。
pub struct UpdateLog {
    path: PathBuf,
    snapshot_path: PathBuf,
    tmp_snapshot_path: PathBuf,
    corrupt_dir: PathBuf,
    chat_id: uuid::Uuid,
    watermark: std::sync::Arc<WatermarkStore>,
    fsync_mode: FsyncMode,
    compact_threshold_bytes: u64,
    compact_interval: Duration,
    degraded: std::sync::Arc<DegradedFlag>,
    file: Option<File>,
    bytes: u64,
    records: u64,
    last_epoch: u32,
    last_seq: u64,
    last_compact_at: Option<SystemTime>,
    snapshot_invalid: Option<String>,
    /// open 时尾部探测是否发现损坏（供 recover 编排决定是否可按快照点直接
    /// 截断日志；损坏必须交给 replay 走完整信号路径）。
    tail_probe_corrupted: bool,
    /// 最近一次 replay 产生的 corrupt 段路径（供 recover 编排聚合）。
    replay_corrupt_artifacts: Vec<PathBuf>,
}

impl UpdateLog {
    /// 打开（或创建）chat 的 update 日志。`watermark` 供 append 成功后
    /// 更新水位（§6 更新时机，顺序 = 日志 fsync → 水位）。
    pub fn open(
        chat_dir: &Path,
        chat_id: uuid::Uuid,
        watermark: std::sync::Arc<WatermarkStore>,
        fsync_mode: FsyncMode,
        compact_threshold_bytes: u64,
        compact_interval: Duration,
        degraded: std::sync::Arc<DegradedFlag>,
    ) -> Result<Self, StoreError> {
        let corrupt_dir = chat_dir.join(CORRUPT_DIR);
        fs::create_dir_all(&corrupt_dir).map_err(|e| StoreError::Io {
            path: corrupt_dir.clone(),
            source: e,
        })?;
        let path = chat_dir.join(UPDATES_LOG_FILE);
        let file = OpenOptions::new()
            .read(true)
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| {
                StoreError::Io {
                    path: path.clone(),
                    source: e,
                }
            })?;
        }
        let snapshot_path = chat_dir.join(UPDATES_SNAPSHOT_FILE);
        let last_compact_at = fs::metadata(&snapshot_path)
            .ok()
            .and_then(|m| m.modified().ok());
        let mut log = UpdateLog {
            path,
            snapshot_path,
            tmp_snapshot_path: chat_dir.join(UPDATES_SNAPSHOT_TMP_FILE),
            corrupt_dir,
            chat_id,
            watermark,
            fsync_mode,
            compact_threshold_bytes,
            compact_interval,
            degraded,
            file: Some(file),
            bytes: 0,
            records: 0,
            last_epoch: 0,
            last_seq: 0,
            last_compact_at,
            snapshot_invalid: None,
            tail_probe_corrupted: false,
            replay_corrupt_artifacts: Vec::new(),
        };
        // 打开时探测尾部，恢复内存计数（重启场景；损坏由 replay 处理）。
        if let Err(e) = log.probe_tail() {
            warn!(chat_id = %log.chat_id, path = %log.path.display(), error = %e, "update log tail probe failed");
            log.degraded.set(format!("update log tail probe failed: {e}"));
        }
        Ok(log)
    }

    /// 探测文件尾，恢复 bytes/records/last_seq 计数，并检测日志是否完整
    /// （完整 = 全部记录通过 CRC/结构校验）。探测只检测不处理：检测到损坏
    /// 停止探测并置 `tail_probe_corrupted`（损坏的截断/信号由
    /// [`UpdateLog::replay`] 负责）。
    ///
    /// 调用方（recover 编排）依赖该标志：**仅当探测完整**时才允许按快照点
    /// 直接截断日志；否则截断会绕过 replay 的损坏检测，把损坏点之后的完好
    /// 记录静默清空（§8.4 告警/degraded/诊断保留契约被绕过）。
    fn probe_tail(&mut self) -> Result<(), String> {
        let file = self.file.as_mut().ok_or("file closed")?;
        let mut pos = 0u64;
        loop {
            file.seek(io::SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
            let mut header = [0u8; 8];
            match file.read_exact(&mut header) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.to_string()),
            }
            let len = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
            if len > MAX_RECORD_BYTES {
                self.tail_probe_corrupted = true;
                break;
            }
            let mut body = vec![0u8; len as usize];
            if let Err(e) = file.read_exact(&mut body) {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    // 尾部半截记录（崩溃残留）→ 损坏。
                    self.tail_probe_corrupted = true;
                    break;
                }
                return Err(e.to_string());
            }
            // CRC 校验：probe 与 replay 同纪律（§4.1）。
            if crc32fast::hash(&body) != u32::from_le_bytes(header[4..8].try_into().expect("4 bytes")) {
                self.tail_probe_corrupted = true;
                break;
            }
            match decode_doc_commit(&self.chat_id, &body) {
                Ok((epoch, seq, _)) => {
                    self.bytes = pos + 8 + len as u64;
                    self.records += 1;
                    self.last_epoch = epoch;
                    self.last_seq = seq;
                }
                Err(_) => {
                    self.tail_probe_corrupted = true;
                    break;
                }
            }
            pos += 8 + len as u64;
        }
        Ok(())
    }

    /// 追加一个逻辑提交并落盘（§4.3）。返回 = 本提交已 durable（PerCommit
    /// 模式）。
    ///
    /// 顺序：append blob → `file.sync_data()`（PerCommit）→ watermark 更新
    /// （§6）→ 返回。Batch 模式不 fsync（Ack 语义降级由 channel 层声明），
    /// 由 [`UpdateLog::flush`] 统一落盘。
    ///
    /// 落盘失败（磁盘满等）→ 置 degraded + `tracing::warn!`，绝不静默
    /// （§8.4）；水位写失败同源（degraded + Err）。
    pub async fn append(
        &mut self,
        epoch: u32,
        seq: u64,
        docs: &[(DocId, &[u8])],
    ) -> Result<(), StoreError> {
        let started = std::time::Instant::now();
        if self.degraded.is_set() {
            return Err(StoreError::Degraded {
                reason: self
                    .degraded
                    .reason()
                    .unwrap_or_else(|| "unknown".into()),
            });
        }
        let body = encode_doc_commit(epoch, seq, docs)?;
        let file = self.file.as_mut().ok_or_else(|| StoreError::Io {
            path: self.path.clone(),
            source: io::Error::new(io::ErrorKind::NotConnected, "log closed"),
        })?;
        if let Err(e) = write_blob(file, &body) {
            self.degraded.set(format!("update log append failed: {e}"));
            warn!(
                chat_id = %self.chat_id, epoch, seq, error = %e,
                "update log append io failed; store degraded"
            );
            return Err(StoreError::Io {
                path: self.path.clone(),
                source: e,
            });
        }
        if self.fsync_mode == FsyncMode::PerCommit {
            if let Err(e) = file.sync_data() {
                self.degraded.set(format!("update log fsync failed: {e}"));
                warn!(
                    chat_id = %self.chat_id, epoch, seq, error = %e,
                    "update log fsync failed; store degraded"
                );
                return Err(StoreError::Io {
                    path: self.path.clone(),
                    source: e,
                });
            }
        }
        // 防御性检查：同 epoch 内 seq 倒退 → 告警（不阻断；聚合器幂等兜底，
        // §4.4）。
        if epoch == self.last_epoch && seq < self.last_seq {
            warn!(
                chat_id = %self.chat_id, epoch, seq, last_seq = self.last_seq,
                "update log seq regressed within same epoch"
            );
        }
        self.bytes += 8 + body.len() as u64;
        self.records += 1;
        self.last_epoch = epoch;
        if seq > self.last_seq {
            self.last_seq = seq;
        }
        if let Err(e) = self.watermark.write(&Watermark { epoch, last_seq: seq }) {
            self.degraded.set(format!("watermark write failed: {e}"));
            warn!(
                chat_id = %self.chat_id, epoch, seq, error = %e,
                "watermark write failed; store degraded"
            );
            return Err(e);
        }
        debug!(
            chat_id = %self.chat_id, epoch, seq,
            bytes = 8 + body.len(), elapsed_ms = started.elapsed().as_millis() as u64,
            "update log append ok"
        );
        Ok(())
    }

    /// 启动回放（§4.4）：顺序读取全部记录；遇损坏（CRC 失败/越界/结构非法/
    /// version 不符）：截断于损坏点，损坏点至 EOF 字节写入
    /// `corrupt/<file>.<offset>.bin`，告警 + degraded。
    ///
    /// 同 epoch 内 seq 非递减防御性校验：违反 → `SeqNonMonotonic` 告警
    /// （不阻断）。
    pub fn replay(&mut self) -> Result<ReplayOutcome, StoreError> {
        self.replay_corrupt_artifacts.clear();
        // 重置内存计数：probe_tail 已在 open 时计数一次，replay 是权威重扫，
        // 避免双计数（bytes/records/last_seq/last_epoch 以回放为准）。
        self.bytes = 0;
        self.records = 0;
        self.last_epoch = 0;
        self.last_seq = 0;
        let path = self.path.clone();
        let mut f = OpenOptions::new()
            .read(true)
            .open(&path)
            .map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?;
        let mut outcome = ReplayOutcome::default();
        let mut pos = 0u64;
        let mut prev: Option<(u32, u64)> = None;
        loop {
            f.seek(io::SeekFrom::Start(pos)).map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?;
            match read_blob(&mut f) {
                Ok(Some(body)) => {
                    let blob_len = 8 + body.len() as u64;
                    match decode_doc_commit(&self.chat_id, &body) {
                        Ok((epoch, seq, docs)) => {
                            if let Some((pe, ps)) = prev {
                                if pe == epoch && seq < ps {
                                    warn!(
                                        chat_id = %self.chat_id, epoch, seq, prev_seq = ps,
                                        "replay: seq non-monotonic within epoch"
                                    );
                                    outcome.warnings.push(
                                        crate::persist::RecoveryWarning {
                                            code: crate::persist::WarningCode::SeqNonMonotonic,
                                            path: path.clone(),
                                            message: format!(
                                                "seq {seq} < previous {ps} within epoch {epoch}"
                                            ),
                                        },
                                    );
                                }
                            }
                            prev = Some((epoch, seq));
                            self.bytes = pos + blob_len;
                            self.records += 1;
                            self.last_epoch = epoch;
                            if seq > self.last_seq {
                                self.last_seq = seq;
                            }
                            outcome.records.push(LogRecord { epoch, seq, docs });
                            pos += blob_len;
                        }
                        Err(detail) => {
                            let info = self.handle_corruption(pos, &detail)?;
                            outcome.truncated = Some(info);
                            outcome.degraded = true;
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(BlobReadError::Corrupt(detail)) => {
                    let info = self.handle_corruption(pos, &detail)?;
                    outcome.truncated = Some(info);
                    outcome.degraded = true;
                    break;
                }
                Err(BlobReadError::Io(e)) => {
                    return Err(StoreError::Io {
                        path: path.clone(),
                        source: e,
                    });
                }
            }
        }
        Ok(outcome)
    }

    /// 处理损坏点：截断日志于损坏点、损坏段写入 corrupt/、置 degraded。
    fn handle_corruption(&mut self, offset: u64, detail: &str) -> Result<CorruptionInfo, StoreError> {
        let path = self.path.clone();
        let mut f = OpenOptions::new()
            .read(true)
            .open(&path)
            .map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?;
        let total = f
            .metadata()
            .map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?
            .len();
        let bytes_kept = total.saturating_sub(offset);
        let mut segment = Vec::with_capacity(bytes_kept as usize);
        f.seek(io::SeekFrom::Start(offset))
            .map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?;
        f.read_to_end(&mut segment)
            .map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?;
        let artifact = self
            .corrupt_dir
            .join(format!("{}.{offset}.bin", UPDATES_LOG_FILE));
        fs::write(&artifact, &segment).map_err(|e| StoreError::Io {
            path: artifact.clone(),
            source: e,
        })?;
        // corrupt 段含 yjs 字节（§8.4 诊断保留），权限 0600（§9.1）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&artifact, fs::Permissions::from_mode(0o600)).map_err(|e| {
                StoreError::Io {
                    path: artifact.clone(),
                    source: e,
                }
            })?;
        }
        self.replay_corrupt_artifacts.push(artifact.clone());
        // 截断日志于损坏点（§4.4）。
        let file = self.file.as_mut().ok_or_else(|| StoreError::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::NotConnected, "log closed"),
        })?;
        file.set_len(offset)
            .map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?;
        file.sync_data().map_err(|e| StoreError::Io {
            path: path.clone(),
            source: e,
        })?;
        self.degraded.set(format!("update log tail truncated at {offset}: {detail}"));
        warn!(
            chat_id = %self.chat_id, path = %path.display(),
            offset, bytes_kept, reason = detail,
            "update log tail truncated; corrupt segment preserved"
        );
        Ok(CorruptionInfo {
            offset,
            bytes_kept,
            reason: detail.to_string(),
        })
    }

    /// 加载 compact 快照（§4.4）。快照 CRC/解析失败 → 移入 `corrupt/` +
    /// degraded + `Ok(None)`（纯日志回放，失效原因经
    /// [`UpdateLog::snapshot_invalid_reason`] 报告）。不存在 → `Ok(None)`。
    pub fn load_snapshot(&mut self) -> Result<Option<Snapshot>, StoreError> {
        let path = self.snapshot_path.clone();
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(StoreError::Io {
                    path: path.clone(),
                    source: e,
                })
            }
        };
        match read_blob(&mut f) {
            Ok(Some(body)) => match serde_json::from_slice::<SnapshotFile>(&body) {
                Ok(file) => {
                    if file.v != BLOB_VERSION {
                        let reason = "snapshot version mismatch";
                        self.snapshot_invalid = Some(reason.to_string());
                        self.degraded.set(reason);
                        warn!(
                            chat_id = %self.chat_id, path = %path.display(),
                            version = file.v,
                            "snapshot version mismatch; treating as invalid"
                        );
                        self.move_snapshot_to_corrupt(reason)?;
                        return Ok(None);
                    }
                    Ok(Some(Snapshot {
                        last_epoch: file.last_epoch,
                        last_applied_seq: file.last_applied_seq,
                        docs: file.docs,
                    }))
                }
                Err(e) => {
                    let reason = format!("snapshot parse failed: {e}");
                    self.snapshot_invalid = Some(reason.clone());
                    self.degraded.set("snapshot json parse failed");
                    warn!(
                        chat_id = %self.chat_id, path = %path.display(), error = %e,
                        "snapshot parse failed; treating as invalid"
                    );
                    self.move_snapshot_to_corrupt(&reason)?;
                    Ok(None)
                }
            },
            Ok(None) => {
                let reason = "empty snapshot file";
                self.snapshot_invalid = Some(reason.to_string());
                self.degraded.set(reason);
                warn!(
                    chat_id = %self.chat_id, path = %path.display(),
                    "snapshot file empty; treating as invalid"
                );
                self.move_snapshot_to_corrupt(reason)?;
                Ok(None)
            }
            Err(BlobReadError::Corrupt(detail)) => {
                let reason = format!("snapshot crc failed: {detail}");
                self.snapshot_invalid = Some(reason.clone());
                self.degraded.set("snapshot crc failed");
                warn!(
                    chat_id = %self.chat_id, path = %path.display(), reason = detail,
                    "snapshot crc failed; treating as invalid"
                );
                self.move_snapshot_to_corrupt(&reason)?;
                Ok(None)
            }
            Err(BlobReadError::Io(e)) => Err(StoreError::Io {
                path: path.clone(),
                source: e,
            }),
        }
    }

    /// 快照失效原因（`Ok(None)` 且此值非 None = 快照损坏/解析失败；供
    /// recover 编排追加 `SnapshotInvalid` 告警，§8）。
    pub fn snapshot_invalid_reason(&self) -> Option<&str> {
        self.snapshot_invalid.as_deref()
    }

    /// 最近一次 [`UpdateLog::replay`] 产生的 corrupt 段路径（§8.4 诊断保留；
    /// 供 recover 编排聚合进 [`RecoveryResult::corrupt_artifacts`]）。
    pub fn replay_corrupt_artifacts(&self) -> &[PathBuf] {
        &self.replay_corrupt_artifacts
    }

    /// open 时尾部探测是否发现损坏（CRC/结构）。为 false 且
    /// `last_seq ≤ 快照点` 时，recover 编排可按快照基线直接截断日志
    /// （§8 崩溃时序 B）；为 true 时必须走 [`UpdateLog::replay`] 的完整
    /// 损坏信号路径（截断 + 告警 + degraded + corrupt 段保留）。
    pub fn tail_probe_corrupted(&self) -> bool {
        self.tail_probe_corrupted
    }

    /// 失效快照移入 corrupt/（诊断保留，§8.4）。
    fn move_snapshot_to_corrupt(&mut self, reason: &str) -> Result<(), StoreError> {
        let artifact = self
            .corrupt_dir
            .join(format!("{}.invalid.bin", UPDATES_SNAPSHOT_FILE));
        fs::rename(&self.snapshot_path, &artifact).map_err(|e| StoreError::Io {
            path: self.snapshot_path.clone(),
            source: e,
        })?;
        debug!(
            chat_id = %self.chat_id, artifact = %artifact.display(), reason,
            "invalid snapshot moved to corrupt"
        );
        Ok(())
    }

    /// compact 触发检查 + 执行（§8 契约）。快照内容（双 Doc 全量 state
    /// update）由调用方 doc-manager 提供。
    ///
    /// 触发条件：日志大小 > `compact_threshold_bytes`，或距上次 compact >
    /// `compact_interval`（§16 默认 64MB/24h）。
    pub async fn maybe_compact(
        &mut self,
        docs: std::collections::HashMap<DocId, Vec<u8>>,
    ) -> Result<bool, StoreError> {
        let by_size = self.bytes > self.compact_threshold_bytes;
        let by_age = match self.last_compact_at {
            Some(t) => t.elapsed().map(|e| e > self.compact_interval).unwrap_or(false),
            None => false,
        };
        if !by_size && !by_age {
            return Ok(false);
        }
        self.compact(docs).await?;
        Ok(true)
    }

    /// compact 原子流程（§8，持写锁调用；快照内容由调用方提供）：
    ///
    /// ```text
    /// 1. 记快照点 s = 当前 last_seq（锁内无并发 append）
    /// 2. 写 updates.snapshot.tmp（单条 blob：{v, lastEpoch, lastAppliedSeq=s,
    ///    createdAt, docs}）
    /// 3. fsync(tmp) → fsync(目录)
    /// 4. rename(tmp → updates.snapshot) → fsync(目录)   ← 原子点
    /// 5. truncate(updates.log, 0) → fsync
    /// ```
    pub async fn compact(
        &mut self,
        docs: std::collections::HashMap<DocId, Vec<u8>>,
    ) -> Result<(), StoreError> {
        let started = std::time::Instant::now();
        if self.degraded.is_set() {
            return Err(StoreError::Degraded {
                reason: self
                    .degraded
                    .reason()
                    .unwrap_or_else(|| "unknown".into()),
            });
        }
        let s = self.last_seq;
        let epoch = self.last_epoch;
        let snapshot = SnapshotFile {
            v: BLOB_VERSION,
            last_epoch: epoch,
            last_applied_seq: s,
            created_at: chrono::Utc::now(),
            docs,
        };
        let body = serde_json::to_vec(&snapshot)
            .map_err(|e| StoreError::Corrupt {
                path: self.snapshot_path.clone(),
                detail: format!("snapshot serialize failed: {e}"),
            })?;
        let tmp_path = self.tmp_snapshot_path.clone();
        let snapshot_path = self.snapshot_path.clone();
        let mut tmp = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            })?;
        // 文件权限 0600（§8.4/§9.1；tmp 继承 umask 默认 0644，rename 前修正，
        // 快照含双 Doc state update 不得被本机其他用户读取）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600)) {
                self.degraded.set(format!("snapshot tmp chmod failed: {e}"));
                warn!(
                    chat_id = %self.chat_id, error = %e,
                    "snapshot tmp chmod failed; store degraded"
                );
                return Err(StoreError::Io {
                    path: tmp_path.clone(),
                    source: e,
                });
            }
        }
        if let Err(e) = write_blob(&mut tmp, &body) {
            self.degraded.set(format!("snapshot tmp write failed: {e}"));
            warn!(
                chat_id = %self.chat_id, error = %e,
                "snapshot tmp write failed; store degraded"
            );
            return Err(StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            });
        }
        if let Err(e) = tmp.sync_all() {
            self.degraded.set(format!("snapshot tmp fsync failed: {e}"));
            warn!(
                chat_id = %self.chat_id, error = %e,
                "snapshot tmp fsync failed; store degraded"
            );
            return Err(StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            });
        }
        drop(tmp);
        sync_dir(self.path.parent().expect("chat dir"))?;
        // 原子点：rename。
        if let Err(e) = fs::rename(&tmp_path, &snapshot_path) {
            self.degraded.set(format!("snapshot rename failed: {e}"));
            warn!(
                chat_id = %self.chat_id, error = %e,
                "snapshot rename failed; store degraded"
            );
            return Err(StoreError::Io {
                path: snapshot_path.clone(),
                source: e,
            });
        }
        sync_dir(self.path.parent().expect("chat dir"))?;
        // truncate 旧日志。
        let file = self.file.as_mut().ok_or_else(|| StoreError::Io {
            path: self.path.clone(),
            source: io::Error::new(io::ErrorKind::NotConnected, "log closed"),
        })?;
        if let Err(e) = file.set_len(0) {
            self.degraded.set(format!("log truncate failed: {e}"));
            warn!(
                chat_id = %self.chat_id, error = %e,
                "log truncate failed; store degraded"
            );
            return Err(StoreError::Io {
                path: self.path.clone(),
                source: e,
            });
        }
        if let Err(e) = file.sync_data() {
            self.degraded.set(format!("log truncate fsync failed: {e}"));
            warn!(
                chat_id = %self.chat_id, error = %e,
                "log truncate fsync failed; store degraded"
            );
            return Err(StoreError::Io {
                path: self.path.clone(),
                source: e,
            });
        }
        self.bytes = 0;
        self.records = 0;
        self.last_compact_at = Some(SystemTime::now());
        debug!(
            chat_id = %self.chat_id, epoch, last_applied_seq = s,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "update log compact ok"
        );
        Ok(())
    }

    /// 批量落盘（Batch 模式）：日志 + 水位统一 fsync（§4.3/§16）。
    pub fn flush(&mut self) -> Result<(), StoreError> {
        let file = self.file.as_mut().ok_or_else(|| StoreError::Io {
            path: self.path.clone(),
            source: io::Error::new(io::ErrorKind::NotConnected, "log closed"),
        })?;
        let degraded = self.degraded.clone();
        if let Err(e) = file.sync_data() {
            degraded.set(format!("update log flush failed: {e}"));
            warn!(
                chat_id = %self.chat_id, error = %e,
                "update log flush failed; store degraded"
            );
            return Err(StoreError::Io {
                path: self.path.clone(),
                source: e,
            });
        }
        self.watermark.flush().map_err(|e| {
            degraded.set(format!("watermark flush failed: {e}"));
            e
        })
    }

    /// 统计（§17.1 指标）。一致性依赖调用方串行化（外层 Mutex）。
    pub fn stats(&self) -> UpdateLogStats {
        UpdateLogStats {
            bytes: self.bytes,
            records: self.records,
            last_seq: self.last_seq,
            last_epoch: self.last_epoch,
        }
    }

    /// 当前是否 degraded（日志侧损坏/落盘失败）。
    pub fn degraded(&self) -> bool {
        self.degraded.is_set()
    }

    /// 当前最大 seq（补推起点计算用，与水位对齐后的结果在
    /// [`WatermarkStore::current`]）。
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// 当前 epoch。
    pub fn last_epoch(&self) -> u32 {
        self.last_epoch
    }

    /// 截断日志至空（快照已就绪时由 recover 调用：快照点 ≥ 日志尾部 seq，
    /// 快照为基线，旧日志可弃，§8 崩溃时序 B 恢复）。
    pub fn truncate_after_snapshot(&mut self) -> Result<(), StoreError> {
        let file = self.file.as_mut().ok_or_else(|| StoreError::Io {
            path: self.path.clone(),
            source: io::Error::new(io::ErrorKind::NotConnected, "log closed"),
        })?;
        file.set_len(0).map_err(|e| StoreError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        file.sync_data().map_err(|e| StoreError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        sync_dir(self.path.parent().expect("chat dir"))?;
        self.bytes = 0;
        self.records = 0;
        Ok(())
    }

    /// 日志文件路径（诊断/测试）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 是否打开（replay 后保持打开，供后续 append）。
    pub fn is_open(&self) -> bool {
        self.file.is_some()
    }
}

/// 目录 fsync（§8.4：创建/rename 文件后对目录做 fsync）。
pub(crate) fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        let f = File::open(dir).map_err(|e| StoreError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
        f.sync_all().map_err(|e| StoreError::Io {
            path: dir.to_path_buf(),
            source: e,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}
