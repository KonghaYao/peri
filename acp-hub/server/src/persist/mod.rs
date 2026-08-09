//! 持久化层（Feature F3）：update 日志、command outbox、(epoch, last_seq) 水位。
//!
//! 承载三种并列的持久化实体（架构 §8.4），三者之间**不提供跨文件原子性**
//! （§8.4.1）：持久化单元是单文件内的单条记录（blob：`len:u32 LE + crc32:u32 LE
//! + body`），跨文件一致性靠恢复不变量顺序（§8.4.1）达成，不靠事务。
//!
//! 目录布局（`docs/plans/f3-persist.md` §2，根目录 0700、文件 0600，§8.4）：
//!
//! ```text
//! <data_dir>/
//! ├── chats/<chat_id>/
//! │   ├── updates.log        # 投影 update 追加日志（blob 记录）
//! │   ├── updates.snapshot   # compact 全量快照（单条 blob 记录）
//! │   ├── outbox.log         # command outbox 追加日志（blob+JSON 记录）
//! │   ├── watermark.json     # (epoch, last_seq) 单条 blob 记录
//! │   └── corrupt/           # 损坏段保留（诊断，§8.4）
//! └── archive/<chat_id>/  # 归档（§9.3，M1 简化）
//! ```
//!
//! 边界声明（不在本层实现）：提交点纪律编排（§4.4）属
//! `channel/command-coordinator`；Doc 补齐（不变量 3）属 `state/doc-manager`；
//! instance 对账后开门（不变量 4）属 `channel` + instance 注册表；`degraded` 的
//! 对外呈现（Registry Doc `global.status`，§17.2）属 `state`。本层只提供数据
//! 源与恢复完成信号。
//!
//! 脱敏纪律（§9.3/协作纪律）：日志字段只记 `chat_id/epoch/seq/bytes/
//! elapsed_ms/error/command_id/verdict` 等元数据，**不记 yjs 字节、正文、
//! token、密钥**。
//!
//! 设计稿：`docs/plans/f3-persist.md`；权威：`docs/architecture.md`
//! §4.4/§4.5.1/§8.4/§8.4.1/§8.5/§16/§17.2。

pub mod outbox;
pub mod store;
pub mod update_log;
pub mod watermark;

#[cfg(test)]
#[cfg(test)]
mod repro_test;
#[cfg(test)]
mod store_test;
#[cfg(test)]
mod update_log_test;
#[cfg(test)]
mod watermark_test;
#[cfg(test)]
mod outbox_test;

use std::path::PathBuf;

/// 数据目录默认位置（§16：`~/.local/share/acp-hub/`，0600）。
pub fn default_data_dir() -> PathBuf {
    dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("acp-hub")
}

/// 持久化配置（§16 默认值；`data_dir` 默认 [`default_data_dir`]）。
///
/// 由 `config::Config` 映射而来（`From<&Config>`），`outbox_retention`
/// （§4.4「chat 关闭后保留 7 天」）config 表暂无对应字段，取默认 7 天
/// 【决策：等待 F2 配置表增补后经 `From<&Config>` 接续】。
#[derive(Debug, Clone)]
pub struct PersistConfig {
    /// 数据目录（§16，默认 `~/.local/share/acp-hub/`）。
    pub data_dir: PathBuf,
    /// fsync 模式（§16，默认 PerCommit；Batch 需显式声明并降级 Ack 语义）。
    pub fsync_mode: crate::config::FsyncMode,
    /// compact 触发字节阈值（§16/§8.4，默认 64MB）。
    pub compact_threshold_bytes: u64,
    /// compact 触发最长时间（§16/§8.4，默认 24h）。
    pub compact_interval: std::time::Duration,
    /// 数据目录磁盘预算（§16/§8.4，默认 2GB）。
    pub disk_budget: u64,
    /// outbox 终态记录保留期（§4.4，默认 7 天）。
    pub outbox_retention: std::time::Duration,
    /// 归档保留时长（§16/§8.4，默认 90 天，开放问题 3）。
    pub archive_retention: std::time::Duration,
}

impl Default for PersistConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            fsync_mode: crate::config::FsyncMode::PerCommit,
            compact_threshold_bytes: 64 * 1024 * 1024,
            compact_interval: std::time::Duration::from_secs(24 * 3600),
            disk_budget: 2 * 1024 * 1024 * 1024,
            outbox_retention: std::time::Duration::from_secs(7 * 86_400),
            archive_retention: std::time::Duration::from_secs(90 * 86_400),
        }
    }
}

impl From<&crate::config::Config> for PersistConfig {
    fn from(cfg: &crate::config::Config) -> Self {
        PersistConfig {
            data_dir: cfg.data_dir.clone(),
            fsync_mode: cfg.fsync_mode,
            compact_threshold_bytes: cfg.compact_trigger_bytes as u64,
            compact_interval: cfg.compact_max_age,
            disk_budget: cfg.disk_budget_bytes as u64,
            outbox_retention: std::time::Duration::from_secs(7 * 86_400),
            archive_retention: cfg.archive_retention,
        }
    }
}

/// 持久化层错误（§3.1）。全部携带定位信息（path/字段），Display 脱敏。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// I/O 失败（含 fsync 失败）。
    #[error("io error on {path}: {source}")]
    Io {
        /// 涉及文件路径。
        path: PathBuf,
        /// 底层 I/O 错误。
        source: std::io::Error,
    },
    /// 记录损坏：CRC 失败 / 结构非法 / len 越界 / version 不符。
    #[error("corrupt record in {path}: {detail}")]
    Corrupt {
        /// 涉及文件路径。
        path: PathBuf,
        /// 损坏细节（脱敏，无正文）。
        detail: String,
    },
    /// outbox 非法状态迁移（设计稿 §5.2 迁移表之外）。
    #[error("invalid outbox transition {from} -> {to} for command {command_id}")]
    InvalidTransition {
        /// 命令 id。
        command_id: uuid::Uuid,
        /// 迁移前状态。
        from: outbox::OutboxStatus,
        /// 迁移目标状态。
        to: outbox::OutboxStatus,
    },
    /// 重发穿透防护：同 commandId 已存在（§4.4 去重表）。
    #[error("duplicate command {command_id} already in state {state}")]
    DuplicateCommand {
        /// 命令 id。
        command_id: uuid::Uuid,
        /// 已存在记录的状态。
        state: outbox::OutboxStatus,
    },
    /// chat 不存在（目录未建/未恢复）。
    #[error("chat {chat_id} not found")]
    ChatNotFound {
        /// chat id。
        chat_id: uuid::Uuid,
    },
    /// outbox 记录不存在（迁移/查询目标 commandId 无记录）。
    /// 【决策】设计稿 §3.1 未列；迁移 API 的记录不存在场景需稳定表达，
    /// 不改动已有变体。
    #[error("command {command_id} not found in outbox")]
    CommandNotFound {
        /// 命令 id。
        command_id: uuid::Uuid,
    },
    /// 已 degraded，拒绝新 committed 承诺（§8.4：落盘失败语义同源）。
    #[error("persist store is degraded: {reason}")]
    Degraded {
        /// degraded 原因（脱敏）。
        reason: String,
    },
    /// 磁盘预算超限（§8.4/§9.2）。
    #[error("disk budget exceeded: used {used}B > limit {limit}B")]
    BudgetExceeded {
        /// 已用字节。
        used: u64,
        /// 上限字节。
        limit: u64,
    },
}

/// 恢复告警码（§8.4.1 不变量 1-2 的告警分类；不阻塞，仅告警）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningCode {
    /// update 日志/outbox 日志尾部截断（§8.4）。
    TailTruncated,
    /// 水位文件损坏（CRC 失败等）。
    WatermarkCorrupt,
    /// 水位与日志尾部 seq 不一致（以较小者为准，§8.4.1 不变量 2）。
    SeqMismatch,
    /// 水位 epoch 与日志不一致（以水位为准，§4.5.1）。
    EpochMismatch,
    /// 同 epoch 内 seq 非单调（防御性，§4.4）。
    SeqNonMonotonic,
    /// 快照无效（CRC/解析失败，移 corrupt/，§8）。
    SnapshotInvalid,
}

/// 恢复告警（§3.2）。字段脱敏。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryWarning {
    /// 告警码。
    pub code: WarningCode,
    /// 相关文件路径。
    pub path: PathBuf,
    /// 脱敏说明。
    pub message: String,
}

/// 恢复编排聚合结果（§8.4.1 不变量 1-2；任一不变量失败 → degraded，§17.2）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryResult {
    /// 任一不变量失败 / 任一文件损坏（§17.2）。由 [`Store::status`] 反映。
    pub degraded: bool,
    /// 截断 / 对齐 / epoch 告警（不阻塞，仅告警）。
    pub warnings: Vec<RecoveryWarning>,
    /// 尾部截断总字节数（§17.1 指标）。
    pub truncated_total_bytes: u64,
    /// 保留的损坏段与失效快照（`corrupt/` 下，诊断，§8.4）。
    pub corrupt_artifacts: Vec<PathBuf>,
}

/// 运行期持久化状态（§7 不变量 5 数据源；Registry `global.status` 消费）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistStatus {
    /// 是否 degraded（启动恢复不变量失败 / 运行期落盘失败，§17.2）。
    pub degraded: bool,
    /// degraded 原因（首个触发原因，脱敏）。
    pub reason: Option<String>,
    /// 数据目录磁盘占用（chats/ + archive/，§9.2 记账范围）。
    pub disk_used: u64,
    /// 磁盘预算上限（§16，默认 2GB）。
    pub disk_limit: u64,
}

/// degraded 信号：Store 级汇聚（`AtomicBool` + 首个原因，§7）。
///
/// 各子存储（UpdateLog/OutboxStore/WatermarkStore）共享同一实例；任一路径
/// 触发即整层 degraded。`reason` 保留首个触发原因（后续触发仅覆盖告警日志，
/// 不覆盖原因，便于诊断根因）。
#[derive(Debug)]
pub struct DegradedFlag {
    flag: std::sync::atomic::AtomicBool,
    reason: std::sync::Mutex<Option<String>>,
}

impl DegradedFlag {
    /// 置 degraded（幂等；首个原因保留）。`tracing::warn!` 由调用方发出。
    pub fn set(&self, reason: impl Into<String>) {
        let reason = reason.into();
        let mut guard = self.reason.lock().expect("degraded reason lock poisoned");
        if guard.is_none() {
            *guard = Some(reason);
        }
        self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// 是否已 degraded。
    pub fn is_set(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 首个触发原因。
    pub fn reason(&self) -> Option<String> {
        self.reason
            .lock()
            .expect("degraded reason lock poisoned")
            .clone()
    }

    /// 新建未触发实例。
    pub fn new() -> Self {
        Self {
            flag: std::sync::atomic::AtomicBool::new(false),
            reason: std::sync::Mutex::new(None),
        }
    }
}

impl Default for DegradedFlag {
    fn default() -> Self {
        Self::new()
    }
}

pub use store::{BudgetReport, EvictionCandidate, ChatStore, Store};
pub use update_log::{
    CorruptionInfo, LogRecord, ReplayOutcome, Snapshot, UpdateLog, UpdateLogStats,
};
pub use watermark::{AlignmentWarning, Watermark, WatermarkStore};
