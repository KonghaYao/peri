//! 进入会话时的恢复契约：范围读取、恢复报告与失败分类（设计 §4.2）。
//!
//! 恢复是**业务上的会话激活动作**，不是读方法：历史展示与元数据读取都不调用
//! [`SessionRecovery::enter`]。范围与候选是纯数据，不含 SQL、连接或事务；失败保留
//! [`SessionResourceError`] 的原始分类，不提前字符串化。
//!
//! [`RecoveryScope`] 只回答「这个根的子树里哪些 thread 仍为 `working`」，不授权启动；
//! 逐个复核、收件地址、提醒接纳与启动由 Host 侧的恢复协调完成。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ports::PeripheralTasks;
use crate::session_resources::{SessionResourceError, SessionResourceErrorKind};
use crate::thread::ThreadId;

/// 进入会话时的恢复入口。
///
/// 调用方必须已经完成会话装配（`SessionState` 已发布）并在**生命周期锁之外**调用；
/// 实现方负责与本进程既有的执行协调合并重复进入。装配冷 child 运行实例的调用方
/// 还要在放行执行前补齐 TaskManager 绑定与在线 endpoint；本 trait 不依赖传输层。
#[async_trait]
pub trait SessionRecovery: Send + Sync {
    async fn enter(&self, thread_id: &ThreadId) -> Result<RecoveryReport, RecoveryError>;
}

/// 一个根子树里的恢复候选：只含候选判定所需的事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCandidate {
    pub thread_id: ThreadId,
    pub parent_thread_id: Option<ThreadId>,
    /// 最近一次有效上下文占用；`None` 表示未知，不是零占用。
    pub context_usage_tokens: Option<u64>,
}

/// 根 thread 及其子树中仍为 `working` 的候选（按创建时间升序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryScope {
    pub root_thread_id: ThreadId,
    pub candidates: Vec<RecoveryCandidate>,
}

/// 进入根时待收敛的遗留 child：`agent_status = active` 且工作意图已落到
/// `idle`/`stopped`。工作意图保持原值，只把运行状态收尾为终态。
///
/// 这只是候选事实：本进程活实例、close 未结清等排除由 Host 侧按运行期事实过滤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedThread {
    pub thread_id: ThreadId,
    /// 只会是 `Idle` 或 `Stopped`。
    pub work_state: crate::thread::WorkState,
}

impl RecoveryScope {
    /// 空范围：根自身就是全部，没有候选。
    pub fn idle(root_thread_id: ThreadId) -> Self {
        Self {
            root_thread_id,
            candidates: Vec::new(),
        }
    }
}

/// 一次 `enter` 的结果。它是调用结果，不是持久化的恢复记录。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub root_thread_id: ThreadId,
    /// 本次实际进入的 thread（可能位于根子树内，不等于根）。
    pub entered_thread_id: ThreadId,
    /// 已有实例：连接现有运行，不重复注入。
    pub reused: Vec<ThreadId>,
    /// 实际注入了恢复提醒、需要启动执行的 thread。
    pub recovered: Vec<ThreadId>,
    /// 保留数据、不启动执行的 thread 及原因。
    pub skipped: Vec<RecoverySkip>,
    /// 逐项准备失败：只记日志与诊断提醒，不失败请求。
    pub failures: Vec<RecoveryFailure>,
    /// 恢复的**根** thread 按直接 owner 查询到的外围存活任务（设计 §6）。
    ///
    /// 冷 child 各自按自己的直接 owner 查询，结果只写进它自己的提醒（同一份
    /// 快照不能替多个 thread 表达归属），因此本字段不汇总它们。
    /// [`PeripheralTasks::Unknown`] 表示本次无法确认，**不是**「没有任务」；
    /// 只有 `Known(vec![])` 才表示已确认没有存活任务。
    pub peripheral_tasks: PeripheralTasks,
}

/// 保持不运行的 thread 与原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySkip {
    pub thread_id: ThreadId,
    pub reason: RecoverySkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoverySkipReason {
    /// 复核时已不是 `working`：不自动续跑。
    Idle,
    /// 进程内已有显式 Stop 意图：不 `allow`、不注入。
    Stopped,
    /// 需要活父执行才能建立的 child 实例，本次进入没有。
    ///
    /// Host 恢复改为独立装配冷 child，不再产生该分类；保留取值供 agent 侧路径复用。
    ParentNotLive,
    /// 冷 child 按 `session/load` 同一路径装配失败：保留数据，本次不启动。
    AssemblyFailed,
    /// 准备期间会话代次变化：旧准备不得落到新生命周期。
    Superseded,
    /// 本进程已有准备中/已启动的恢复，或 MQ 仍有未消费的 entry 提醒。
    AlreadyRunning,
}

impl RecoverySkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Stopped => "stopped",
            Self::ParentNotLive => "parent_not_live",
            Self::AssemblyFailed => "assembly_failed",
            Self::Superseded => "superseded",
            Self::AlreadyRunning => "already_running",
        }
    }
}

impl std::fmt::Display for RecoverySkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 逐项失败：thread 已保留数据，只是本次没能完成恢复准备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryFailure {
    pub thread_id: ThreadId,
    pub kind: RecoveryErrorKind,
    /// 分类之外的定位信息；不含 SQL、连接或令牌。
    pub detail: String,
}

/// 恢复失败的稳定分类；出口按它映射 ACP 错误码，不解析文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryErrorKind {
    NotFound,
    /// 父链缺失或成环、存储内容不可解释。
    Corrupt,
    /// 本次打开只有读权限：恢复需要写入提醒，不能放行执行。
    ReadOnly,
    Unavailable,
    Unsupported,
    /// 准备期间会话代次变化。
    Superseded,
}

impl RecoveryErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Corrupt => "corrupt",
            Self::ReadOnly => "read_only",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::Superseded => "superseded",
        }
    }

    /// 该失败是否使会话无法执行：恢复需要读写归属、frozen 与提醒接纳。
    ///
    /// `Unavailable` / `Superseded` 仍可由用户显式继续或下一次进入重试；
    /// `Unsupported` 只表示后端不提供恢复查询，不等于不可执行。
    pub fn blocks_execution(self) -> bool {
        matches!(self, Self::Corrupt | Self::ReadOnly | Self::NotFound)
    }
}

impl std::fmt::Display for RecoveryErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 进入会话的恢复失败：稳定分类、可定位 thread，以及原始存储失败。
#[derive(Debug)]
pub struct RecoveryError {
    kind: RecoveryErrorKind,
    thread_id: Option<ThreadId>,
    source: SessionResourceError,
}

impl RecoveryError {
    pub fn new(
        kind: RecoveryErrorKind,
        thread_id: Option<ThreadId>,
        source: SessionResourceError,
    ) -> Self {
        Self {
            kind,
            thread_id,
            source,
        }
    }

    /// 准备期间会话代次变化：不是存储失败，仍然是「旧准备不落到新生命周期」。
    pub fn superseded(thread_id: &ThreadId) -> Self {
        Self::new(
            RecoveryErrorKind::Superseded,
            Some(thread_id.clone()),
            SessionResourceError::new(SessionResourceErrorKind::Conflict {
                detail: "session generation changed during recovery".to_owned(),
            }),
        )
    }

    pub fn kind(&self) -> RecoveryErrorKind {
        self.kind
    }

    pub fn thread_id(&self) -> Option<&ThreadId> {
        self.thread_id.as_ref()
    }

    pub fn source(&self) -> &SessionResourceError {
        &self.source
    }

    /// 补充可定位的 thread（范围读取失败时调用方知道问的是哪一个）。
    pub fn with_thread(mut self, thread_id: &ThreadId) -> Self {
        if self.thread_id.is_none() {
            self.thread_id = Some(thread_id.clone());
        }
        self
    }
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(thread_id) = &self.thread_id {
            write!(f, " ({thread_id})")?;
        }
        write!(f, ": {}", self.source)
    }
}

impl std::error::Error for RecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<SessionResourceError> for RecoveryError {
    fn from(source: SessionResourceError) -> Self {
        let kind = match source.kind() {
            SessionResourceErrorKind::NotFound => RecoveryErrorKind::NotFound,
            SessionResourceErrorKind::Corrupt { .. } => RecoveryErrorKind::Corrupt,
            SessionResourceErrorKind::ReadOnlyStore => RecoveryErrorKind::ReadOnly,
            SessionResourceErrorKind::Unsupported => RecoveryErrorKind::Unsupported,
            // 余下分类没有「根/环/只读」之外的恢复语义：保留 source 原文，按暂不可用处理。
            _ => RecoveryErrorKind::Unavailable,
        };
        Self::new(kind, None, source)
    }
}

/// 一次进入会话的恢复结果投影（wire 形状；不是持久记录）。
///
/// 会话装配与历史回放成功后，恢复准备失败仍以成功响应返回，结果放在
/// `_meta.peri.recovery`。`status = failed` 且 `execution_blocked = true` 时，
/// 服务端同时拒绝该会话的 prompt 执行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryView {
    pub status: RecoveryViewStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_thread_id: Option<ThreadId>,
    pub entered_thread_id: ThreadId,
    pub recovered: Vec<ThreadId>,
    pub reused: Vec<ThreadId>,
    pub skipped: Vec<RecoverySkipView>,
    pub failures: Vec<RecoveryFailureView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RecoveryFailureView>,
    pub execution_blocked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryViewStatus {
    Ok,
    /// 逐项准备失败：相关会话保留，但不自动续跑。
    Partial,
    /// 进入会话的恢复准备本身失败。
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySkipView {
    pub thread_id: ThreadId,
    pub reason: RecoverySkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryFailureView {
    pub thread_id: ThreadId,
    pub kind: RecoveryErrorKind,
    pub detail: String,
    pub execution_blocked: bool,
}

impl RecoveryView {
    pub fn from_report(report: &RecoveryReport) -> Self {
        let status = if report.failures.is_empty() {
            RecoveryViewStatus::Ok
        } else {
            RecoveryViewStatus::Partial
        };
        Self {
            status,
            root_thread_id: Some(report.root_thread_id.clone()),
            entered_thread_id: report.entered_thread_id.clone(),
            recovered: report.recovered.clone(),
            reused: report.reused.clone(),
            skipped: report
                .skipped
                .iter()
                .map(|skip| RecoverySkipView {
                    thread_id: skip.thread_id.clone(),
                    reason: skip.reason,
                })
                .collect(),
            failures: report
                .failures
                .iter()
                .map(RecoveryFailureView::from_failure)
                .collect(),
            error: None,
            execution_blocked: false,
        }
    }

    pub fn from_error(entered_thread_id: &ThreadId, error: &RecoveryError) -> Self {
        let failure = RecoveryFailureView {
            thread_id: error.thread_id().cloned().unwrap_or_else(|| {
                // 存储没给出 thread 时仍以本次进入的会话定位；不猜其他会话。
                entered_thread_id.clone()
            }),
            kind: error.kind(),
            detail: error.source().to_string(),
            execution_blocked: error.kind().blocks_execution(),
        };
        Self {
            status: RecoveryViewStatus::Failed,
            root_thread_id: error.thread_id().cloned(),
            entered_thread_id: entered_thread_id.clone(),
            recovered: Vec::new(),
            reused: Vec::new(),
            skipped: Vec::new(),
            failures: Vec::new(),
            error: Some(failure),
            execution_blocked: error.kind().blocks_execution(),
        }
    }

    /// 普通 idle load 不产生恢复噪声：只有实际注入或失败才附加。
    pub fn should_attach(&self) -> bool {
        self.status != RecoveryViewStatus::Ok
            || !self.recovered.is_empty()
            || !self.failures.is_empty()
    }
}

impl RecoveryFailureView {
    fn from_failure(failure: &RecoveryFailure) -> Self {
        Self {
            thread_id: failure.thread_id.clone(),
            kind: failure.kind,
            detail: failure.detail.clone(),
            execution_blocked: failure.kind.blocks_execution(),
        }
    }
}

#[cfg(test)]
#[path = "recovery_test.rs"]
mod tests;
