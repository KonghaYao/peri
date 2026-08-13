//! command outbox（§4.4/§5）：commandId 去重账本 + 状态机迁移 API。
//!
//! 持久化形态（设计稿 §5.1【决策】）：`outbox.log` 为**追加式状态快照日志**
//! ——每次状态迁移追加一条**完整记录**（JSON body，blob 外壳包裹，后者覆盖
//! 前者）；删除 = 追加 tombstone 记录（`{v, commandId, status: "removed"}`）。
//! 启动重放顺序应用（insert/update/remove）重建 `Map<command_id, record>`。
//! 物理压缩（重写文件）只在清理时发生（§5.5）。
//!
//! 状态机迁移表（设计稿 §5.2；非法迁移一律 [`StoreError::InvalidTransition`]
//! 拒绝并 `tracing::warn`，不静默）：
//!
//! | from | to | 触发方 |
//! |------|-----|--------|
//! | received | accepted | coordinator 入队（两阶段 Ack 之 accepted） |
//! | accepted | intent_durable | 意图落盘（§4.4 提交点纪律第一步） |
//! | intent_durable | dispatched | 下发 instance（置 `dispatched_at`） |
//! | intent_durable | （tombstone） | retryable 失败（§4.4：清除允许重发） |
//! | received / accepted / intent_durable | failed | 非 retryable 失败 |
//! | dispatched | delivery_confirmed | L1+L2 达成（M1 合并） |
//! | dispatched | delivery_unknown | L2 后 L3 不可得（M1 路径 B，§5.3） |
//! | delivery_confirmed | projection_committed | 投影 update 落盘后 |
//! | delivery_confirmed | failed | 业务失败（客户端收 action_error） |
//! | projection_committed | completed | committed Ack 返回（终态） |
//! | delivery_unknown | completed | 人工裁决「确认已送达」（§5.3 runbook） |
//! | delivery_unknown | （tombstone） | 人工裁决「确认未送达」 |
//! | delivery_unknown | delivery_unknown | 裁决「仍未知」（幂等，重载不推进） |
//!
//! **H1 裁决**（主管补充，设计稿缺口）：投递后（dispatched 及之后）收到
//! **retryable** 失败 → 状态**回退**到 `intent_durable`（记录保留、去重索引
//! 不删、`dispatched_at` 清除、状态标记可重发）；非 retryable 失败 →
//! `failed`。retryable 分类以架构 §4.4 为准（`AGENT_UNAVAILABLE` /
//! `INSTANCE_OFFLINE`）。投递前（received/accepted/intent_durable）的
//! retryable 失败 → tombstone 清除（允许重发重新执行，设计稿 §5.2 原语义）。

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use uuid::Uuid;

use acp_hub_proto::action::PermissionDecision;

use crate::config::FsyncMode;

use crate::persist::update_log::{read_blob, write_blob, BlobReadError, CORRUPT_DIR};
use crate::persist::{DegradedFlag, StoreError};

/// outbox 日志文件名（§2 目录布局）。
pub const OUTBOX_LOG_FILE: &str = "outbox.log";
/// outbox 物理压缩临时文件名（§5.5；崩溃残留由 recover 步骤 1 清理）。
pub const OUTBOX_LOG_TMP_FILE: &str = "outbox.log.tmp";

/// outbox 记录 JSON 版本（tombstone 记录头 `v`，§5.1）。
pub const OUTBOX_JSON_VERSION: u8 = 0x01;

/// 命令类型（§4.8 M1 五种；JSON 形态与 proto action `type` 对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandType {
    /// `chat/create`（以 chat_id 为天然幂等键，§4.5）。
    #[serde(rename = "chat/create")]
    Create,
    /// `chat/prompt`（非幂等，禁止盲重试）。
    #[serde(rename = "chat/prompt")]
    Prompt,
    /// `chat/cancel`（非幂等）。
    #[serde(rename = "chat/cancel")]
    Cancel,
    /// `chat/close`（以 chat_id 为天然幂等键）。
    #[serde(rename = "chat/close")]
    Close,
    /// `permission/resolve`（非幂等）。
    #[serde(rename = "permission/resolve")]
    Resolve,
}

impl CommandType {
    /// 命令固有幂等性分类（§5.2 分类表）：create/close → SafeToRedeliver；
    /// prompt/cancel/resolve → NoAutoRedeliver。调用方仍可显式覆盖。
    pub fn default_retryable_class(self) -> RetryableClass {
        match self {
            CommandType::Create | CommandType::Close => RetryableClass::SafeToRedeliver,
            CommandType::Prompt | CommandType::Cancel | CommandType::Resolve => {
                RetryableClass::NoAutoRedeliver
            }
        }
    }
}

/// 命令固有幂等性分类（§4.4 顾问3）：进入 outbox 前必须显式分类，未分类
/// 默认 [`RetryableClass::NoAutoRedeliver`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryableClass {
    /// 可安全重发（以 chat_id 等为天然幂等键）。
    SafeToRedeliver,
    /// 禁止自动重发（非幂等，路径 B：仅可对账/人工裁决后重发，§5.3）。
    NoAutoRedeliver,
}

/// outbox 状态机状态（§5.1/§5.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxStatus {
    /// 已入队（两阶段 Ack 之 accepted）。
    Received,
    /// 已 accepted。
    Accepted,
    /// 意图已落盘（§4.4 提交点纪律第一步）。
    IntentDurable,
    /// 已下发 instance（置 `dispatched_at`）。
    Dispatched,
    /// L1+L2 达成（M1 合并，§4.4）。
    DeliveryConfirmed,
    /// 投影 update 已落盘。
    ProjectionCommitted,
    /// committed Ack 已返回（终态）。
    Completed,
    /// 非 retryable 失败（终态）。
    Failed,
    /// L2 后 L3 不可得（M1 路径 B，§5.3）。
    DeliveryUnknown,
}

impl OutboxStatus {
    /// 是否为终态（completed/failed；清理策略与归档前置条件检查用，§5.5）。
    pub fn is_terminal(self) -> bool {
        matches!(self, OutboxStatus::Completed | OutboxStatus::Failed)
    }
}

impl std::fmt::Display for OutboxStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            OutboxStatus::Received => "received",
            OutboxStatus::Accepted => "accepted",
            OutboxStatus::IntentDurable => "intent_durable",
            OutboxStatus::Dispatched => "dispatched",
            OutboxStatus::DeliveryConfirmed => "delivery_confirmed",
            OutboxStatus::ProjectionCommitted => "projection_committed",
            OutboxStatus::Completed => "completed",
            OutboxStatus::Failed => "failed",
            OutboxStatus::DeliveryUnknown => "delivery_unknown",
        };
        f.write_str(s)
    }
}

/// 最近一次失败（`delivery_unknown` 对账展示，§4.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastError {
    /// 稳定错误码（§4.4，如 `AGENT_UNAVAILABLE`；脱敏）。
    pub code: String,
    /// 是否 retryable（架构 §4.4 分类：`AGENT_UNAVAILABLE`/`INSTANCE_OFFLINE`）。
    pub retryable: bool,
    /// 失败时刻（server 时钟，§4.7）。
    pub at: DateTime<Utc>,
}

impl LastError {
    /// 由 proto 稳定错误码构造（retryable 分类事实源：
    /// [`ErrorCode::default_retryable`]）。
    pub fn from_error_code(code: acp_hub_proto::ack::ErrorCode) -> Self {
        LastError {
            code: serde_json::to_value(code)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{code:?}")),
            retryable: code.default_retryable(),
            at: Utc::now(),
        }
    }
}

/// outbox 记录（§5.1；JSON 字段与 §4.4 `commandId → {type, turnId, status,
/// dispatched_at}` 一一对应 + 补充字段）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxRecord {
    /// 命令 id（幂等键，同 chat 唯一）。
    pub command_id: Uuid,
    /// 所属 chat。
    pub chat_id: Uuid,
    /// 命令类型。
    pub command_type: CommandType,
    /// turn id（server 生成；同 commandId 重试复用，§4.4）。
    pub turn_id: Option<Uuid>,
    /// 当前状态。
    pub status: OutboxStatus,
    /// 命令固有幂等性分类（§4.4 顾问3）。
    pub retryable_class: RetryableClass,
    /// 下发时刻（`dispatched` 后非 None）。
    pub dispatched_at: Option<DateTime<Utc>>,
    /// 创建时刻（server 时钟，§4.7）。
    pub created_at: DateTime<Utc>,
    /// 最近迁移时刻。
    pub updated_at: DateTime<Utc>,
    /// 最近失败（回退/重试展示）。
    pub last_error: Option<LastError>,
    /// 投递尝试次数（§17.1 指标；每次进入 `dispatched` +1）。
    pub attempt_count: u32,
    /// 非幂等命令在明确未投递时安全恢复所需的最小证据。
    /// 可选以保持旧 outbox 记录的向后兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Box<CommandRecovery>>,
}

/// 按命令类型封闭的恢复证据。不得存放 bearer token、Cookie 或
/// 用户消息正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandRecovery {
    /// 官方 ACP `session/request_permission` 的 JSON-RPC response 回投材料。
    PermissionResponse {
        /// server 生成的权限投影身份。
        permission_id: String,
        /// agent request id，响应必须原样回显。
        request_id: serde_json::Value,
        /// ACP 官方 options，用于将 Allow/Deny 映射回 optionId。
        options: Vec<serde_json::Value>,
        /// 首次裁决；必须与重试 action 完全一致。
        decision: PermissionDecision,
    },
}

/// 新记录（[`OutboxStore::insert`] 入参 → Received）。
#[derive(Debug, Clone)]
pub struct NewOutboxRecord {
    /// 命令 id。
    pub command_id: Uuid,
    /// 所属 chat。
    pub chat_id: Uuid,
    /// 命令类型。
    pub command_type: CommandType,
    /// turn id（可选，§4.4：重试复用同一 turnId）。
    pub turn_id: Option<Uuid>,
    /// 幂等性分类（未显式分类默认 NoAutoRedeliver，§4.4 顾问3）。
    pub retryable_class: RetryableClass,
}

/// delivery_unknown 人工裁决（§5.3 runbook；权限与依据属 control 层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryVerdict {
    /// 确认已送达 → completed。
    ConfirmedDelivered,
    /// 确认未送达 → tombstone 清除（允许重发）。
    ConfirmedNotDelivered,
    /// 仍未知 → 保持 delivery_unknown（幂等，重载不推进）。
    StillUnknown,
}

/// outbox.log 重放条目（§5.2 replay 入参）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxLogEntry {
    /// 完整记录（insert 或 update，后者覆盖前者）。
    Record(OutboxRecord),
    /// tombstone（删除）。
    Remove(Uuid),
}

/// tombstone 记录 JSON（§5.1：`{v, commandId, status: "removed"}`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tombstone {
    v: u8,
    command_id: Uuid,
    status: TombstoneStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TombstoneStatus {
    #[serde(rename = "removed")]
    Removed,
}

/// 磁盘条目（untagged：先试完整记录，`status: "removed"` 落到 tombstone）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum DiskEntry {
    Record(OutboxRecord),
    Tombstone(Tombstone),
}

/// 重放统计（§5.4）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayStats {
    /// 新增记录数。
    pub inserted: usize,
    /// 覆盖（更新）记录数。
    pub updated: usize,
    /// tombstone 删除数。
    pub removed: usize,
}

/// outbox 重放结果（§5.4，与 update 日志同纪律：损坏 → 尾部截断 + 告警 +
/// degraded）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutboxReplayResult {
    /// 重放统计。
    pub stats: ReplayStats,
    /// 尾部截断信息。
    pub truncated: Option<crate::persist::update_log::CorruptionInfo>,
    /// 保留的损坏段路径（corrupt/ 下）。
    pub corrupt_artifacts: Vec<PathBuf>,
    /// 日志损坏 → degraded（§8.4 同纪律）。
    pub degraded: bool,
}

/// 清理统计（§5.5）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupStats {
    /// 删除（tombstone）的终态记录数。
    pub removed: usize,
    /// 是否发生了物理压缩（重写 outbox.log）。
    pub compressed: bool,
    /// 压缩前文件字节。
    pub bytes_before: u64,
    /// 压缩后文件字节。
    pub bytes_after: u64,
}

/// command outbox 存储（§5）：追加式状态快照日志 + 内存去重索引。
///
/// `&mut self` 方法必须在调用方持外层 `tokio::sync::Mutex` 后调用（设计稿
/// §10 并发模型）。
pub struct OutboxStore {
    path: PathBuf,
    corrupt_dir: PathBuf,
    fsync_mode: FsyncMode,
    retention: Duration,
    degraded: std::sync::Arc<DegradedFlag>,
    index: HashMap<Uuid, OutboxRecord>,
    file: Option<fs::File>,
}

impl OutboxStore {
    /// 打开（或创建）chat 的 outbox 日志。
    pub fn open(
        chat_dir: &Path,
        fsync_mode: FsyncMode,
        retention: Duration,
        degraded: std::sync::Arc<DegradedFlag>,
    ) -> Result<Self, StoreError> {
        let path = chat_dir.join(OUTBOX_LOG_FILE);
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
        Ok(OutboxStore {
            path,
            corrupt_dir: chat_dir.join(CORRUPT_DIR),
            fsync_mode,
            retention,
            degraded,
            index: HashMap::new(),
            file: Some(file),
        })
    }

    /// 插入新记录 → `Received`（§5.2）。遇已存在 commandId（任意状态）→
    /// [`StoreError::DuplicateCommand`]（重发穿透防护，§4.4）；重发判定与
    /// duplicate Ack 由 coordinator 经 [`OutboxStore::get`] 完成。
    pub fn insert(&mut self, rec: NewOutboxRecord) -> Result<(), StoreError> {
        if let Some(existing) = self.index.get(&rec.command_id) {
            return Err(StoreError::DuplicateCommand {
                command_id: rec.command_id,
                state: existing.status,
            });
        }
        let now = Utc::now();
        let record = OutboxRecord {
            command_id: rec.command_id,
            chat_id: rec.chat_id,
            command_type: rec.command_type,
            turn_id: rec.turn_id,
            status: OutboxStatus::Received,
            retryable_class: rec.retryable_class,
            dispatched_at: None,
            created_at: now,
            updated_at: now,
            last_error: None,
            attempt_count: 0,
            recovery: None,
        };
        self.append_record(&record)
    }

    /// `received → accepted`（§5.2）。
    pub fn mark_accepted(&mut self, id: Uuid) -> Result<(), StoreError> {
        self.transition(id, OutboxStatus::Accepted, |r| {
            r.dispatched_at = None;
        })
    }

    /// `accepted → intent_durable`（意图落盘，§4.4 提交点纪律第一步）。
    pub fn mark_intent_durable(&mut self, id: Uuid) -> Result<(), StoreError> {
        self.transition(id, OutboxStatus::IntentDurable, |_| {})
    }

    /// 为已落盘意图附加恢复证据。证据与 commandId 同一条记录
    /// 追加并按 outbox fsync 纪律落盘，所以重启后仍能验证精确重试。
    pub fn set_recovery(&mut self, id: Uuid, recovery: CommandRecovery) -> Result<(), StoreError> {
        let mut record = self
            .index
            .get(&id)
            .cloned()
            .ok_or_else(|| self.not_found(id))?;
        if record.status != OutboxStatus::IntentDurable {
            return self.reject(id, record.status, OutboxStatus::IntentDurable);
        }
        if let Some(existing) = &record.recovery {
            if existing.as_ref() == &recovery {
                return Ok(());
            }
            return Err(StoreError::Corrupt {
                path: self.path.clone(),
                detail: format!("conflicting recovery evidence for command {id}"),
            });
        }
        record.recovery = Some(Box::new(recovery));
        record.updated_at = Utc::now();
        self.append_record(&record)
    }

    /// 投递已确认后删除不再需要的恢复材料，降低长期保留面。
    pub fn clear_recovery(&mut self, id: Uuid) -> Result<(), StoreError> {
        let mut record = self
            .index
            .get(&id)
            .cloned()
            .ok_or_else(|| self.not_found(id))?;
        if record.status != OutboxStatus::DeliveryConfirmed {
            return self.reject(id, record.status, OutboxStatus::DeliveryConfirmed);
        }
        if record.recovery.is_none() {
            return Ok(());
        }
        record.recovery = None;
        record.updated_at = Utc::now();
        self.append_record(&record)
    }

    /// `intent_durable → dispatched`（下发 instance；置 `dispatched_at`，
    /// attempt_count +1）。此后崩溃 → 重发由 outbox 兜底返回 `duplicate`。
    pub fn mark_dispatched(&mut self, id: Uuid, at: DateTime<Utc>) -> Result<(), StoreError> {
        self.transition(id, OutboxStatus::Dispatched, |r| {
            r.dispatched_at = Some(at);
            r.attempt_count = r.attempt_count.saturating_add(1);
        })
    }

    /// `dispatched → delivery_confirmed`（L1+L2 达成，M1 合并）。
    pub fn mark_delivery_confirmed(&mut self, id: Uuid) -> Result<(), StoreError> {
        self.transition(id, OutboxStatus::DeliveryConfirmed, |_| {})
    }

    /// `delivery_confirmed → projection_committed`（投影 update 落盘后）。
    pub fn mark_projection_committed(&mut self, id: Uuid) -> Result<(), StoreError> {
        self.transition(id, OutboxStatus::ProjectionCommitted, |_| {})
    }

    /// `projection_committed → completed`（committed Ack 返回，终态）。
    pub fn mark_completed(&mut self, id: Uuid) -> Result<(), StoreError> {
        self.transition(id, OutboxStatus::Completed, |_| {})
    }

    /// 失败迁移（§5.2 + **H1 裁决**）：
    ///
    /// - `err.retryable == false`（`INVALID_STATE`/`FORBIDDEN`/`CHAT_NOT_FOUND`
    ///   ，§4.4）→ `failed`（终态）；来源状态为投递前（received/accepted/
    ///   intent_durable）或投递后（dispatched/delivery_confirmed/
    ///   projection_committed）。
    /// - `err.retryable == true`（`AGENT_UNAVAILABLE`/`INSTANCE_OFFLINE`）：
    ///   投递后（dispatched/delivery_confirmed/projection_committed）→
    ///   **回退**到 `intent_durable`（记录保留、去重索引不删、`dispatched_at`
    ///   清除、状态标记可重发）【H1 裁决】；投递前（received/accepted/
    ///   intent_durable）→ tombstone 清除（允许重发重新执行，§5.2 原语义）。
    ///
    /// `delivery_unknown` 不由此迁移（须经 [`OutboxStore::resolve_delivery_unknown`]）；
    /// 终态拒绝。
    pub fn mark_failed(&mut self, id: Uuid, err: LastError) -> Result<(), StoreError> {
        let from = self
            .index
            .get(&id)
            .map(|r| r.status)
            .ok_or_else(|| self.not_found(id))?;
        if from.is_terminal() {
            return self.reject(id, from, OutboxStatus::Failed);
        }
        if from == OutboxStatus::DeliveryUnknown {
            return self.reject(id, from, OutboxStatus::Failed);
        }
        if err.retryable {
            // H1 裁决：投递后回退；投递前 tombstone 清除。
            if matches!(
                from,
                OutboxStatus::Dispatched
                    | OutboxStatus::DeliveryConfirmed
                    | OutboxStatus::ProjectionCommitted
            ) {
                let mut record = self.index.get(&id).expect("checked").clone();
                let chat_id = record.chat_id;
                record.status = OutboxStatus::IntentDurable;
                record.dispatched_at = None;
                record.updated_at = err.at;
                record.last_error = Some(err);
                self.append_record(&record)?;
                tracing::info!(
                    event = "outbox.retryable_fallback", command_id = %id,
                    chat_id = %chat_id,
                    "outbox record fell back to intent_durable for retry"
                );
                return Ok(());
            }
            // 投递前：清除记录允许重发。
            self.tombstone(id)?;
            tracing::info!(
                event = "outbox.clear_for_retry", command_id = %id,
                "outbox record cleared for retry (pre-dispatch retryable failure)"
            );
            return Ok(());
        }
        // 非 retryable → failed（终态）。
        self.transition(id, OutboxStatus::Failed, |r| {
            r.last_error = Some(err);
        })
    }

    /// `dispatched → delivery_unknown`（L2 后 L3 不可得，M1 路径 B，§5.3）。
    pub fn mark_delivery_unknown(&mut self, id: Uuid) -> Result<(), StoreError> {
        self.transition(id, OutboxStatus::DeliveryUnknown, |_| {})
    }

    /// server 重启时收敛带恢复证据的未终态命令。进程内
    /// `intent_durable` 原本可在同一 runtime 恢复，但重启后 ChatRegistry /
    /// binding 均需重建，不得默认原 ACP request 仍可投递。`dispatched`
    /// 更无法确认副作用是否已发生。两者统一进入持久化
    /// `delivery_unknown`，仅可经运维裁决。
    pub fn reconcile_recovery_after_restart(&mut self) -> Result<usize, StoreError> {
        let ids = self
            .index
            .values()
            .filter(|record| {
                record.recovery.is_some()
                    && matches!(
                        record.status,
                        OutboxStatus::IntentDurable | OutboxStatus::Dispatched
                    )
            })
            .map(|record| record.command_id)
            .collect::<Vec<_>>();
        for id in &ids {
            self.transition(*id, OutboxStatus::DeliveryUnknown, |_| {})?;
        }
        Ok(ids.len())
    }

    /// delivery_unknown 人工裁决（§5.3 runbook；审计日志由本方法写入）：
    ///
    /// - `ConfirmedDelivered` → completed；
    /// - `ConfirmedNotDelivered` → tombstone 清除（允许重发）；
    /// - `StillUnknown` → 保持（幂等，重载不推进）。
    pub fn resolve_delivery_unknown(
        &mut self,
        id: Uuid,
        verdict: DeliveryVerdict,
    ) -> Result<(), StoreError> {
        let from = self
            .index
            .get(&id)
            .map(|r| r.status)
            .ok_or_else(|| self.not_found(id))?;
        let chat_id = self.index.get(&id).expect("checked").chat_id;
        if from != OutboxStatus::DeliveryUnknown {
            return self.reject(
                id,
                from,
                match verdict {
                    DeliveryVerdict::ConfirmedDelivered => OutboxStatus::Completed,
                    DeliveryVerdict::ConfirmedNotDelivered | DeliveryVerdict::StillUnknown => {
                        OutboxStatus::DeliveryUnknown
                    }
                },
            );
        }
        tracing::info!(
            event = "outbox.resolve", command_id = %id, chat_id = %chat_id,
            verdict = ?verdict,
            "delivery_unknown resolved by operator"
        );
        match verdict {
            DeliveryVerdict::ConfirmedDelivered => {
                self.transition(id, OutboxStatus::Completed, |_| {})
            }
            DeliveryVerdict::ConfirmedNotDelivered => self.tombstone(id),
            DeliveryVerdict::StillUnknown => Ok(()), // 幂等：重载不推进
        }
    }

    /// 清除记录（tombstone；§5.2：retryable 失败清除 / 裁决「确认未送达」）。
    /// 合法来源：`intent_durable`（投递前）与 `delivery_unknown`（裁决）。
    pub fn clear_for_retry(&mut self, id: Uuid) -> Result<(), StoreError> {
        let from = self
            .index
            .get(&id)
            .map(|r| r.status)
            .ok_or_else(|| self.not_found(id))?;
        if !matches!(
            from,
            OutboxStatus::IntentDurable | OutboxStatus::DeliveryUnknown
        ) {
            return self.reject(id, from, OutboxStatus::IntentDurable);
        }
        self.tombstone(id)
    }

    /// 去重索引查询（重发判定，§4.4/§7 协作表）。
    pub fn get(&self, id: Uuid) -> Option<&OutboxRecord> {
        self.index.get(&id)
    }

    /// 全部记录（清理/归档前置条件检查用，§5.5）。
    pub fn records(&self) -> impl Iterator<Item = &OutboxRecord> {
        self.index.values()
    }

    /// 记录数（归档前置条件「outbox 全终态」检查辅助）。
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// 索引是否为空。
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// 启动重放（§5.2）：顺序应用磁盘条目（insert/update/remove）重建索引。
    pub fn replay(&mut self, entries: impl IntoIterator<Item = OutboxLogEntry>) -> ReplayStats {
        let mut stats = ReplayStats::default();
        for entry in entries {
            match entry {
                OutboxLogEntry::Record(rec) => {
                    if self.index.insert(rec.command_id, rec).is_some() {
                        stats.updated += 1;
                    } else {
                        stats.inserted += 1;
                    }
                }
                OutboxLogEntry::Remove(id) => {
                    if self.index.remove(&id).is_some() {
                        stats.removed += 1;
                    }
                }
            }
        }
        stats
    }

    /// 从磁盘重放（§5.4）：顺序读 `outbox.log`；完整记录 → 索引插入；
    /// tombstone → 删除；损坏 → 尾部截断 + 告警 + degraded（与 update 日志
    /// 同纪律）。
    pub fn replay_from_disk(&mut self) -> Result<OutboxReplayResult, StoreError> {
        let mut result = OutboxReplayResult::default();
        let mut f = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .map_err(|e| StoreError::Io {
                path: self.path.clone(),
                source: e,
            })?;
        let mut entries = Vec::new();
        let mut pos = 0u64;
        loop {
            f.seek(std::io::SeekFrom::Start(pos))
                .map_err(|e| StoreError::Io {
                    path: self.path.clone(),
                    source: e,
                })?;
            match read_blob(&mut f) {
                Ok(Some(body)) => {
                    let blob_len = 8 + body.len() as u64;
                    match serde_json::from_slice::<DiskEntry>(&body) {
                        Ok(DiskEntry::Record(rec)) => entries.push(OutboxLogEntry::Record(rec)),
                        Ok(DiskEntry::Tombstone(t)) => {
                            entries.push(OutboxLogEntry::Remove(t.command_id))
                        }
                        Err(e) => {
                            // JSON 结构非法 → 损坏（§5.4 同纪律）。
                            let artifact =
                                self.handle_corruption(pos, &format!("json parse failed: {e}"))?;
                            result.truncated = Some(artifact.info);
                            result.corrupt_artifacts.push(artifact.path);
                            result.degraded = true;
                            break;
                        }
                    }
                    pos += blob_len;
                }
                Ok(None) => break,
                Err(BlobReadError::Corrupt(detail)) => {
                    let artifact = self.handle_corruption(pos, &detail)?;
                    result.truncated = Some(artifact.info);
                    result.corrupt_artifacts.push(artifact.path);
                    result.degraded = true;
                    break;
                }
                Err(BlobReadError::Io(e)) => {
                    return Err(StoreError::Io {
                        path: self.path.clone(),
                        source: e,
                    });
                }
            }
        }
        result.stats = self.replay(entries);
        Ok(result)
    }

    /// 损坏点处理：损坏段写入 corrupt/ + 截断 + degraded（§5.4 同纪律）。
    fn handle_corruption(
        &mut self,
        offset: u64,
        detail: &str,
    ) -> Result<CorruptionArtifact, StoreError> {
        let mut f = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .map_err(|e| StoreError::Io {
                path: self.path.clone(),
                source: e,
            })?;
        let total = f
            .metadata()
            .map_err(|e| StoreError::Io {
                path: self.path.clone(),
                source: e,
            })?
            .len();
        let bytes_kept = total.saturating_sub(offset);
        let mut segment = Vec::with_capacity(bytes_kept as usize);
        f.seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| StoreError::Io {
                path: self.path.clone(),
                source: e,
            })?;
        f.read_to_end(&mut segment).map_err(|e| StoreError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        let artifact = self
            .corrupt_dir
            .join(format!("{}.{offset}.bin", OUTBOX_LOG_FILE));
        fs::write(&artifact, &segment).map_err(|e| StoreError::Io {
            path: artifact.clone(),
            source: e,
        })?;
        // corrupt 段权限 0600（§9.1；fs::write 默认继承 umask）。
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
        let file = self.file.as_mut().ok_or_else(|| StoreError::Io {
            path: self.path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotConnected, "outbox closed"),
        })?;
        file.set_len(offset).map_err(|e| StoreError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        file.sync_data().map_err(|e| StoreError::Io {
            path: self.path.clone(),
            source: e,
        })?;
        self.degraded
            .set(format!("outbox log tail truncated at {offset}: {detail}"));
        warn!(
            path = %self.path.display(), offset, bytes_kept, reason = detail,
            "outbox log tail truncated; corrupt segment preserved"
        );
        Ok(CorruptionArtifact {
            path: artifact,
            info: crate::persist::update_log::CorruptionInfo {
                offset,
                bytes_kept,
                reason: detail.to_string(),
            },
        })
    }

    /// 清理策略（§5.5）：终态（completed/failed）记录自 `updated_at` 起保留
    /// `retention`（默认 7 天），期满且 `chat_closed` → tombstone + 物理
    /// 压缩。`chat_closed` 由 control 层在 close 完成时经
    /// [`crate::persist::ChatStore::mark_closed`] 记录；instance 注销判断由
    /// 调用方保证（persist 只校验自己可校验的部分）。
    ///
    /// 压缩：重写 `outbox.log`（临时文件 → fsync → rename → 目录 fsync），
    /// 与 compact 同纪律（§8）；在 Mutex 内进行。
    pub fn cleanup(&mut self, now: DateTime<Utc>, chat_closed: bool) -> CleanupStats {
        let mut stats = CleanupStats::default();
        if !chat_closed {
            return stats;
        }
        let expired: Vec<Uuid> = self
            .index
            .iter()
            .filter(|(_, r)| r.status.is_terminal())
            .filter(|(_, r)| {
                r.updated_at + chrono::Duration::from_std(self.retention).unwrap_or_default() <= now
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            // tombstone 追加失败 → degraded（不静默）；压缩照常进行（索引已
            // 同步更新，崩溃后重放由 tombstone 兜底）。
            if let Err(e) = self.tombstone(id) {
                warn!(
                    command_id = %id, error = %e,
                    "outbox cleanup tombstone failed"
                );
            } else {
                stats.removed += 1;
            }
        }
        if stats.removed > 0 {
            stats.bytes_before = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
            match self.compact_log() {
                Ok(()) => {
                    stats.compressed = true;
                    stats.bytes_after = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
                }
                Err(e) => warn!(
                    error = %e,
                    "outbox cleanup compaction failed"
                ),
            }
        }
        stats
    }

    /// 物理压缩：重写 outbox.log 只保留索引中存活记录（§5.5）。
    fn compact_log(&mut self) -> Result<(), StoreError> {
        let path = self.path.clone();
        let tmp_path = self.path.with_file_name(OUTBOX_LOG_TMP_FILE);
        let mut tmp = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            })?;
        // 文件权限 0600（§9.1；tmp 继承 umask 默认 0644，rename 前修正）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600)).map_err(|e| {
                StoreError::Io {
                    path: tmp_path.clone(),
                    source: e,
                }
            })?;
        }
        for rec in self.index.values() {
            let body = serde_json::to_vec(rec).map_err(|e| StoreError::Corrupt {
                path: path.clone(),
                detail: format!("outbox record serialize failed: {e}"),
            })?;
            write_blob(&mut tmp, &body).map_err(|e| StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            })?;
        }
        if let Err(e) = tmp.sync_all() {
            self.degraded
                .set(format!("outbox compaction tmp fsync failed: {e}"));
            warn!(error = %e, "outbox compaction tmp fsync failed; store degraded");
            return Err(StoreError::Io {
                path: tmp_path.clone(),
                source: e,
            });
        }
        drop(tmp);
        if let Err(e) = fs::rename(&tmp_path, &path) {
            self.degraded
                .set(format!("outbox compaction rename failed: {e}"));
            warn!(error = %e, "outbox compaction rename failed; store degraded");
            return Err(StoreError::Io {
                path: path.clone(),
                source: e,
            });
        }
        crate::persist::update_log::sync_dir(self.path.parent().expect("chat dir"))?;
        // 重建追加句柄（rename 后旧句柄指向已删除 inode）。
        let file = OpenOptions::new()
            .read(true)
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| StoreError::Io {
                path: path.clone(),
                source: e,
            })?;
        self.file = Some(file);
        Ok(())
    }

    /// 追加一条完整记录并落盘（§5.1：每次迁移 = 追加 + fsync，PerCommit）。
    fn append_record(&mut self, record: &OutboxRecord) -> Result<(), StoreError> {
        let body = serde_json::to_vec(record).map_err(|e| StoreError::Corrupt {
            path: self.path.clone(),
            detail: format!("outbox record serialize failed: {e}"),
        })?;
        self.append_body(&body, record.command_id)?;
        self.index.insert(record.command_id, record.clone());
        Ok(())
    }

    /// tombstone：追加删除记录（§5.1）+ 索引删除。
    fn tombstone(&mut self, id: Uuid) -> Result<(), StoreError> {
        let body = serde_json::to_vec(&Tombstone {
            v: OUTBOX_JSON_VERSION,
            command_id: id,
            status: TombstoneStatus::Removed,
        })
        .map_err(|e| StoreError::Corrupt {
            path: self.path.clone(),
            detail: format!("tombstone serialize failed: {e}"),
        })?;
        self.append_body(&body, id)?;
        self.index.remove(&id);
        Ok(())
    }

    /// 写 blob + fsync（PerCommit；Batch 延迟到 [`OutboxStore::flush`]）。
    fn append_body(&mut self, body: &[u8], command_id: Uuid) -> Result<(), StoreError> {
        let file = self.file.as_mut().ok_or_else(|| StoreError::Io {
            path: self.path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotConnected, "outbox closed"),
        })?;
        let degraded = self.degraded.clone();
        if let Err(e) = write_blob(file, body) {
            degraded.set(format!("outbox append failed: {e}"));
            warn!(
                command_id = %command_id, error = %e,
                "outbox append failed; store degraded"
            );
            return Err(StoreError::Io {
                path: self.path.clone(),
                source: e,
            });
        }
        if self.fsync_mode == FsyncMode::PerCommit {
            if let Err(e) = file.sync_data() {
                degraded.set(format!("outbox fsync failed: {e}"));
                warn!(
                    command_id = %command_id, error = %e,
                    "outbox fsync failed; store degraded"
                );
                return Err(StoreError::Io {
                    path: self.path.clone(),
                    source: e,
                });
            }
        }
        Ok(())
    }

    /// Batch 模式统一落盘（§16；Ack 语义降级由上层声明）。
    pub fn flush(&mut self) -> Result<(), StoreError> {
        let file = self.file.as_mut().ok_or_else(|| StoreError::Io {
            path: self.path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotConnected, "outbox closed"),
        })?;
        let degraded = self.degraded.clone();
        if let Err(e) = file.sync_data() {
            degraded.set(format!("outbox flush failed: {e}"));
            warn!(error = %e, "outbox flush failed; store degraded");
            return Err(StoreError::Io {
                path: self.path.clone(),
                source: e,
            });
        }
        Ok(())
    }

    /// 日志文件路径（诊断/测试）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// degraded 状态（供 Store 聚合，§7）。
    pub fn degraded_is_set(&self) -> bool {
        self.degraded.is_set()
    }

    /// 当前文件字节数（磁盘预算记账）。
    pub fn file_bytes(&self) -> u64 {
        fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    /// 合法迁移执行：校验 + 追加记录 + 更新索引。
    fn transition(
        &mut self,
        id: Uuid,
        to: OutboxStatus,
        mutate: impl FnOnce(&mut OutboxRecord),
    ) -> Result<(), StoreError> {
        let mut record = self
            .index
            .get(&id)
            .cloned()
            .ok_or_else(|| self.not_found(id))?;
        let from = record.status;
        if !allowed_transition(from, to) {
            return self.reject(id, from, to);
        }
        record.status = to;
        record.updated_at = Utc::now();
        mutate(&mut record);
        self.append_record(&record)
    }

    /// 非法迁移：`InvalidTransition` 拒绝（不写盘，§5.2 不静默）。
    fn reject(&self, id: Uuid, from: OutboxStatus, to: OutboxStatus) -> Result<(), StoreError> {
        warn!(
            command_id = %id, from = ?from, to = ?to,
            "invalid outbox transition rejected"
        );
        Err(StoreError::InvalidTransition {
            command_id: id,
            from,
            to,
        })
    }

    /// 记录不存在错误。
    fn not_found(&self, id: Uuid) -> StoreError {
        StoreError::CommandNotFound { command_id: id }
    }
}

/// corruption 归档中间产物。
struct CorruptionArtifact {
    path: PathBuf,
    info: crate::persist::update_log::CorruptionInfo,
}

/// 状态机合法迁移表（§5.2 表 + H1 裁决回退路径经 [`OutboxStore::mark_failed`]
/// 单独处理，不在此表）。
fn allowed_transition(from: OutboxStatus, to: OutboxStatus) -> bool {
    use OutboxStatus::*;
    matches!(
        (from, to),
        (Received, Accepted)
            | (Received, Failed)
            | (Accepted, IntentDurable)
            | (Accepted, Failed)
            | (IntentDurable, Dispatched)
            | (IntentDurable, DeliveryUnknown)
            | (IntentDurable, Failed)
            | (Dispatched, DeliveryConfirmed)
            | (Dispatched, DeliveryUnknown)
            // H1 扩展：投递后非 retryable 失败 → failed（§4.4 重试分类）。
            | (Dispatched, Failed)
            | (DeliveryConfirmed, ProjectionCommitted)
            | (DeliveryConfirmed, Failed)
            | (ProjectionCommitted, Completed)
            // H1 裁决：投影落盘后业务失败（action_error，非 retryable）→
            // failed（终态）；与 mark_failed 注释一致（§4.4 重试分类）。
            | (ProjectionCommitted, Failed)
            | (DeliveryUnknown, Completed)
            | (DeliveryUnknown, DeliveryUnknown)
    )
}
