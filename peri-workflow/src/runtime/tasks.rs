//! Workflow 任务表：agent 任务的发布、状态机、单次终态提交（CAS）与任务事件。
//!
//! 任务表是 per-session、process-local 的运行期状态（不写 journal）；跨进程恢复
//! 不在范围（design §5.1）。键为 `(runId, taskId)`：`taskId` 作用域 = 单次 run，
//! 宿主不假设跨 run 唯一。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::protocol::{AgentRunParams, AgentRunResult};

use super::close_gate::CloseGate;
use super::event_log::BoundedEventLog;
use super::WorkflowResourceUpdate;

/// 终态任务在快照表中的保留窗口（与 progress store 的 run 保留窗口同量级）。
const TASK_RETENTION_MS: i64 = 300_000;
/// 快照表硬上界：超出时从最旧的终态任务开始淘汰（活跃任务不淘汰）。
const MAX_TASK_RECORDS: usize = 4096;

/// 任务基线：未终态任务集合 + 同一临界区读取的已提交上界。
///
/// 资源面随任务事件页返回该基线（`activeTasks` / `headSequence` / `truncatedBefore`），
/// 作为消费者「全量扫描未终态任务」的唯一入口；其正确性依赖「快照与上界同临界区、
/// 上界不因淘汰回退」两条不变量（design §4.3；对抗审查 F3/F4）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskBaseline {
    /// 未终态任务（发布顺序；活跃任务不参与保留窗口淘汰）。
    pub active_tasks: Vec<WorkflowTaskRecord>,
    /// 基线的已提交上界：`sequence <= head_sequence` 的状态变更都在快照中可见。
    pub head_sequence: u64,
    /// 事件保留边界：`sequence < truncated_before` 的事件已不可读。
    pub truncated_before: u64,
}

/// 任务状态机：`published → executing → completed | failed | cancelled`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTaskState {
    Published,
    Executing,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowTaskState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// 任务记录：发布参数 + 当前状态 + 终态结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTaskRecord {
    pub task_id: String,
    pub run_id: String,
    pub agent_id: u64,
    pub state: WorkflowTaskState,
    /// 发布时的完整参数快照（不可变）。
    pub params: AgentRunParams,
    /// 发布时刻（wall clock，毫秒）。
    pub published_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<AgentRunResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 发布结果：首个发布提交；重复发布幂等返回既有记录（不覆盖参数）。
///
/// `run` / `agent` 已被 kill（kill 栅栏先于发布到达）时同样提交，但记录直接落
/// `cancelled` 终态：事件流先 `published` 后 `cancelled`，消费者不得执行它。
///
/// 实例已关闭时发布被**拒绝**（[`PublishOutcome::Closed`]）：不登记记录、不追加
/// 事件（关闭顺序第 ① 步；与 run 准入共享同一关闭门）。
#[derive(Debug)]
pub enum PublishOutcome {
    Published(WorkflowTaskRecord),
    AlreadyKnown(WorkflowTaskRecord),
    /// 实例已关闭：关闭返回后到达的发布一律拒绝（对抗审查 F6）。
    Closed,
}

/// 执行 claim 结果：只有 [`ClaimOutcome::Claimed`] 允许启动执行（唯一 claim）。
///
/// 其他分支说明消费者重试 / kill 先到：调用方不得执行该任务。
#[derive(Debug)]
pub enum ClaimOutcome {
    /// 本次成功 claim：`published → executing`，取消句柄已登记。
    Claimed(WorkflowTaskRecord),
    /// 已被另一个消费者 claim（或有并发 claim 竞争失败）：不得重复执行。
    AlreadyExecuting(WorkflowTaskRecord),
    /// 任务已终态（kill 先于消费 / 已结算）：不得执行。
    AlreadyTerminal(WorkflowTaskRecord),
    /// 任务不存在。
    NotFound,
}

/// 终态提交的准入条件（允许结算的活跃态集合，单一权威）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleAdmission {
    /// 任意活跃态（`published` / `executing`）：内部取消路径使用（kill 覆盖排队与执行中）。
    AnyActive,
    /// 仅 `executing`：外部回填路径使用（回填只接受已被唯一 claim 的任务）。
    ExecutingOnly,
}

/// 终态提交结果（单次终态提交，compare-and-set）。
#[derive(Debug)]
pub enum SettleOutcome {
    /// 本次提交成功进入终态。
    Settled(WorkflowTaskRecord),
    /// 任务已有终态：返回既有记录（重复回填幂等；调用方对结果冲突记录告警）。
    AlreadyTerminal(WorkflowTaskRecord),
    /// 调用方传入非终态 `state`：拒绝提交，记录未变更（调用方 bug，不得静默写入）。
    InvalidState(WorkflowTaskRecord),
    /// 活跃态不满足准入条件（如回填 `published` 任务）：拒绝提交，记录未变更。
    NotAdmitted(WorkflowTaskRecord),
    /// 任务不存在（未知 taskId）。
    NotFound,
}

/// 任务事件类型（`workflow://tasks` 与 `workflow://runs/{runId}/tasks`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTaskEventKind {
    Published,
    Executing,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowTaskEventKind {
    /// 稳定事件名（`eventId` 与 serde 序列化共用的唯一字面量；不得用 `Debug`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::Executing => "executing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl From<WorkflowTaskState> for WorkflowTaskEventKind {
    fn from(state: WorkflowTaskState) -> Self {
        match state {
            WorkflowTaskState::Published => Self::Published,
            WorkflowTaskState::Executing => Self::Executing,
            WorkflowTaskState::Completed => Self::Completed,
            WorkflowTaskState::Failed => Self::Failed,
            WorkflowTaskState::Cancelled => Self::Cancelled,
        }
    }
}

/// 任务事件：追加不可变，携带事件时刻的任务快照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTaskEvent {
    /// 单调 sequence（session 级任务流）。
    pub sequence: u64,
    /// 幂等键：`{runId}:{taskId}:{kind}`。
    pub event_id: String,
    pub run_id: String,
    pub task_id: String,
    pub agent_id: u64,
    pub kind: WorkflowTaskEventKind,
    pub at_ms: i64,
    /// 事件时刻的任务快照（发布事件含参数；终态事件含结果）。
    pub task: WorkflowTaskRecord,
}

/// 终态通知面：任务进入终态（新提交或观察到既有终态）后回调。
///
/// 语义（design §5.1/§5.2 第 4 条）：**先提交任务表，再投递结果**——回调发生在
/// 状态与事件提交之后，调用方（`WorkflowRuntime`）据此向 Node 投递 `agent/result`。
///
/// 契约：回调在任务表临界区内被调用（`commit_locked` / 幂等分支），实现必须
/// **非阻塞**（只做移交，例如 spawn 投递任务），且不得回调进入任务表（无重入）。
pub type TerminalSink = Arc<dyn Fn(&WorkflowTaskRecord) + Send + Sync>;

struct TableInner {
    tasks: HashMap<(String, String), WorkflowTaskEntry>,
    /// 发布顺序（快照排序稳定）。
    order: Vec<(String, String)>,
    /// run 级 kill 栅栏：置位后该 run 新发布的任务直接转 `cancelled`（值 = 取消原因）。
    run_kills: HashMap<String, Option<String>>,
    /// agent 级 kill 栅栏：置位后该 `(run, agent)` 新发布的任务直接转 `cancelled`。
    agent_kills: HashMap<(String, u64), Option<String>>,
}

struct WorkflowTaskEntry {
    record: WorkflowTaskRecord,
    /// 执行中取消句柄（消费者经 [`WorkflowTaskTable::mark_executing`] 登记）。
    cancel: Option<CancellationToken>,
    claim_id: Option<String>,
    /// 进入终态的时刻（wall clock 毫秒；仅用于保留窗口淘汰，不序列化）。
    terminal_at_ms: Option<i64>,
}

/// session 级任务表（含任务事件日志、订阅通知与关闭门）。
pub struct WorkflowTaskTable {
    inner: Mutex<TableInner>,
    events: Arc<BoundedEventLog<WorkflowTaskEvent>>,
    updates: broadcast::Sender<WorkflowResourceUpdate>,
    /// 与 [`super::WorkflowRuntime`] 共享的关闭门：关闭后拒绝新发布（对抗审查 F6）。
    gate: Arc<CloseGate>,
    /// 终态通知（装配期一次绑定；见 [`TerminalSink`] 契约）。
    terminal_sink: parking_lot::RwLock<Option<TerminalSink>>,
}

impl WorkflowTaskTable {
    pub fn new(updates: broadcast::Sender<WorkflowResourceUpdate>, gate: Arc<CloseGate>) -> Self {
        Self {
            inner: Mutex::new(TableInner {
                tasks: HashMap::new(),
                order: Vec::new(),
                run_kills: HashMap::new(),
                agent_kills: HashMap::new(),
            }),
            events: Arc::new(BoundedEventLog::new(Self::EVENT_LOG_CAPACITY)),
            updates,
            gate,
            terminal_sink: parking_lot::RwLock::new(None),
        }
    }

    /// 绑定终态通知（`WorkflowRuntime` 构造期一次绑定；见 [`TerminalSink`] 契约）。
    pub fn set_terminal_sink(&self, sink: TerminalSink) {
        *self.terminal_sink.write() = Some(sink);
    }

    /// 通知终态（幂等路径也调用：重复回填是「可重试投递」的入口，Node 侧按 taskId 去重）。
    fn notify_terminal(&self, record: &WorkflowTaskRecord) {
        if let Some(sink) = self.terminal_sink.read().as_ref() {
            sink(record);
        }
    }

    /// 任务事件日志容量（保留边界随淘汰前进）。
    const EVENT_LOG_CAPACITY: usize = 512;

    /// 任务事件日志（`workflow://tasks` 的事实源）。
    pub fn events(&self) -> &Arc<BoundedEventLog<WorkflowTaskEvent>> {
        &self.events
    }

    /// 发布任务：首个发布提交并追加 `task.published` 事件；重复发布幂等。
    ///
    /// run / agent kill 栅栏已置位时（kill 与发布并发），记录直接落 `cancelled`
    /// 终态：Node 侧的迟到发布不会逃逸出本轮 kill。状态变更与事件追加在同一临界区
    /// 提交（事件顺序 == 状态提交顺序）。
    ///
    /// 关闭门票据覆盖「关闭检查 + 状态提交」整个临界区：关闭返回后到达的发布被拒绝
    /// （不登记、不产生事件），在途发布要么整体在关闭前提交、要么被拒绝
    /// （对抗审查 F6）。
    pub fn publish(&self, task_id: String, params: AgentRunParams) -> PublishOutcome {
        let Some(_ticket) = self.gate.admit() else {
            return PublishOutcome::Closed;
        };
        let run_id = params.run_id.clone();
        let agent_id = params.agent_id;
        let mut inner = self.inner.lock();
        Self::trim_locked(&mut inner, now_ms());
        let key = (run_id.clone(), task_id.clone());
        if let Some(existing) = inner.tasks.get(&key) {
            return PublishOutcome::AlreadyKnown(existing.record.clone());
        }
        let now = now_ms();
        let kill_reason = inner
            .run_kills
            .get(&run_id)
            .cloned()
            .or_else(|| inner.agent_kills.get(&(run_id.clone(), agent_id)).cloned());
        let record = WorkflowTaskRecord {
            task_id: task_id.clone(),
            run_id: run_id.clone(),
            agent_id,
            state: WorkflowTaskState::Published,
            params,
            published_at_ms: now,
            result: None,
            reason: None,
        };
        inner.tasks.insert(
            key.clone(),
            WorkflowTaskEntry {
                record: record.clone(),
                cancel: None,
                claim_id: None,
                terminal_at_ms: None,
            },
        );
        inner.order.push(key.clone());
        self.commit_locked(&mut inner, WorkflowTaskEventKind::Published, &record);
        if let Some(reason) = kill_reason {
            // kill 栅栏命中：同一次临界区内补一条终态事件（published → cancelled）。
            let terminal = self.write_terminal_locked(
                &mut inner,
                &key,
                WorkflowTaskState::Cancelled,
                None,
                Some(reason.unwrap_or_else(|| "workflow run killed".to_string())),
                now,
            );
            if let Some(record) = terminal {
                return PublishOutcome::Published(record);
            }
        }
        PublishOutcome::Published(record)
    }

    /// 登记执行：`published → executing`，登记取消句柄（唯一 claim）。
    ///
    /// 只有返回 [`ClaimOutcome::Claimed`] 的调用方允许启动执行；已被他人 claim、
    /// 已终态或不存在时调用方不得执行（重试路径按返回值收敛）。
    pub fn mark_executing(
        &self,
        run_id: &str,
        task_id: &str,
        cancel: CancellationToken,
    ) -> ClaimOutcome {
        let mut inner = self.inner.lock();
        let key = (run_id.to_string(), task_id.to_string());
        let Some(entry) = inner.tasks.get_mut(&key) else {
            return ClaimOutcome::NotFound;
        };
        match entry.record.state {
            WorkflowTaskState::Published => {
                entry.record.state = WorkflowTaskState::Executing;
                entry.cancel = Some(cancel);
                let record = entry.record.clone();
                self.commit_locked(&mut inner, WorkflowTaskEventKind::Executing, &record);
                ClaimOutcome::Claimed(record)
            }
            WorkflowTaskState::Executing => ClaimOutcome::AlreadyExecuting(entry.record.clone()),
            _ => ClaimOutcome::AlreadyTerminal(entry.record.clone()),
        }
    }

    /// Atomic, retry-safe MCP claim. Capacity and state are decided by the owner.
    pub fn claim_for_consumer(
        &self,
        run_id: &str,
        task_id: &str,
        claim_id: &str,
        capacity: Option<u32>,
    ) -> Option<WorkflowTaskRecord> {
        let mut inner = self.inner.lock();
        let key = (run_id.to_owned(), task_id.to_owned());
        let entry = inner.tasks.get(&key)?;
        if entry.record.state == WorkflowTaskState::Executing
            && entry.claim_id.as_deref() == Some(claim_id)
        {
            return Some(entry.record.clone());
        }
        if entry.record.state != WorkflowTaskState::Published {
            return None;
        }
        if capacity.is_some_and(|limit| {
            inner
                .tasks
                .values()
                .filter(|entry| {
                    entry.record.run_id == run_id
                        && entry.record.state == WorkflowTaskState::Executing
                })
                .count()
                >= limit as usize
        }) {
            return None;
        }
        let entry = inner.tasks.get_mut(&key)?;
        entry.record.state = WorkflowTaskState::Executing;
        entry.claim_id = Some(claim_id.to_owned());
        entry.cancel = Some(CancellationToken::new());
        let record = entry.record.clone();
        self.commit_locked(&mut inner, WorkflowTaskEventKind::Executing, &record);
        Some(record)
    }

    /// 单次终态提交（CAS）：只允许一次进入终态，重复提交返回既有终态。
    ///
    /// 非终态 `state` 直接拒绝（[`SettleOutcome::InvalidState`]），不修改记录——
    /// 不依赖 `debug_assert!` 维持运行时不变量。准入条件由 `admission` 显式声明；
    /// 重复回填携带不同结果时保留首次结果并记录告警（design §5.1）。
    pub fn settle(
        &self,
        run_id: &str,
        task_id: &str,
        admission: SettleAdmission,
        state: WorkflowTaskState,
        result: Option<AgentRunResult>,
        reason: Option<String>,
    ) -> SettleOutcome {
        let mut inner = self.inner.lock();
        let key = (run_id.to_string(), task_id.to_string());
        let Some(entry) = inner.tasks.get_mut(&key) else {
            return SettleOutcome::NotFound;
        };
        if !state.is_terminal() {
            return SettleOutcome::InvalidState(entry.record.clone());
        }
        if entry.record.state.is_terminal() {
            if let Some(incoming) = result.as_ref() {
                if entry.record.result.as_ref() != Some(incoming) {
                    warn!(
                        target: "workflow",
                        run_id,
                        task_id,
                        first_state = ?entry.record.state,
                        incoming_state = ?state,
                        first_result = result_summary(entry.record.result.as_ref()),
                        incoming_result = result_summary(Some(incoming)),
                        "workflow task terminal conflict: keeping the first result (idempotent retry or conflicting late delivery)"
                    );
                }
            }
            let record = entry.record.clone();
            // 重复回填是「可重试投递」的入口（首次投递可能未达 Node）：再投递既有终态，
            // Node 侧按 taskId 去重（design §5.1）。
            self.notify_terminal(&record);
            return SettleOutcome::AlreadyTerminal(record);
        }
        if admission == SettleAdmission::ExecutingOnly
            && entry.record.state != WorkflowTaskState::Executing
        {
            return SettleOutcome::NotAdmitted(entry.record.clone());
        }
        let record = self.write_terminal_locked(&mut inner, &key, state, result, reason, now_ms());
        match record {
            Some(record) => SettleOutcome::Settled(record),
            None => SettleOutcome::NotFound,
        }
    }

    /// 取消单个任务：同一临界区内完成「非终态判断 + 写 cancelled + 取取消句柄」，
    /// 只有本次提交成功的调用方才触发句柄。
    ///
    /// 另一路已先进入终态时返回 `AlreadyTerminal` 且**不**触发取消（句柄仍归
    /// 首次结算方所有）：不存在「token 已取出但从未触发」的窗口。
    pub fn cancel(&self, run_id: &str, task_id: &str, reason: Option<String>) -> SettleOutcome {
        let mut inner = self.inner.lock();
        let key = (run_id.to_string(), task_id.to_string());
        let Some(entry) = inner.tasks.get_mut(&key) else {
            return SettleOutcome::NotFound;
        };
        if entry.record.state.is_terminal() {
            return SettleOutcome::AlreadyTerminal(entry.record.clone());
        }
        let token = entry.cancel.take();
        let record = self.write_terminal_locked(
            &mut inner,
            &key,
            WorkflowTaskState::Cancelled,
            None,
            reason,
            now_ms(),
        );
        drop(inner);
        if let Some(token) = token {
            token.cancel();
        }
        match record {
            Some(record) => SettleOutcome::Settled(record),
            None => SettleOutcome::NotFound,
        }
    }

    /// 取消某 run 下全部未终态任务（run kill / 会话关闭路径），返回受影响 taskId。
    ///
    /// 先原子置 run 级 kill 栅栏，再取未终态任务：kill 期间（Node 仍可能）发布的新
    /// 任务命中栅栏，直接落 `cancelled`，不逃逸出本轮 kill。
    pub fn cancel_run(&self, run_id: &str, reason: Option<String>) -> Vec<String> {
        let mut inner = self.inner.lock();
        inner.run_kills.insert(run_id.to_string(), reason.clone());
        self.cancel_matching_locked(&mut inner, |record| record.run_id == run_id, reason)
    }

    /// 取消某 run 下指定 agent 的全部未终态任务（单 agent kill 路径）。
    ///
    /// 与 [`Self::cancel_run`] 同构：先置 agent 级 kill 栅栏，再取消已登记任务。
    pub fn cancel_agent(&self, run_id: &str, agent_id: u64, reason: Option<String>) -> Vec<String> {
        let mut inner = self.inner.lock();
        inner
            .agent_kills
            .insert((run_id.to_string(), agent_id), reason.clone());
        self.cancel_matching_locked(
            &mut inner,
            |record| record.run_id == run_id && record.agent_id == agent_id,
            reason,
        )
    }

    /// 单次临界区内取消全部命中任务：CAS → 追加事件 → 收集取消句柄（锁外触发）。
    fn cancel_matching_locked(
        &self,
        inner: &mut TableInner,
        predicate: impl Fn(&WorkflowTaskRecord) -> bool,
        reason: Option<String>,
    ) -> Vec<String> {
        let now = now_ms();
        let keys: Vec<(String, String)> = inner
            .order
            .iter()
            .filter(|key| {
                inner.tasks.get(*key).is_some_and(|entry| {
                    !entry.record.state.is_terminal() && predicate(&entry.record)
                })
            })
            .cloned()
            .collect();
        let mut tokens = Vec::new();
        let mut cancelled = Vec::new();
        for key in keys {
            if let Some(entry) = inner.tasks.get_mut(&key) {
                if let Some(token) = entry.cancel.take() {
                    tokens.push(token);
                }
            }
            let Some(record) = self.write_terminal_locked(
                inner,
                &key,
                WorkflowTaskState::Cancelled,
                None,
                reason.clone(),
                now,
            ) else {
                continue;
            };
            cancelled.push(record.task_id);
        }
        for token in tokens {
            token.cancel();
        }
        cancelled
    }

    /// 持锁内的终态写入：更新记录 → 追加事件 → 发订阅唤醒。
    ///
    /// 返回写入后的记录；任务不存在返回 `None`。调用方必须已持有 `inner`。
    fn write_terminal_locked(
        &self,
        inner: &mut TableInner,
        key: &(String, String),
        state: WorkflowTaskState,
        result: Option<AgentRunResult>,
        reason: Option<String>,
        now: i64,
    ) -> Option<WorkflowTaskRecord> {
        let entry = inner.tasks.get_mut(key)?;
        entry.record.state = state;
        entry.record.result = result;
        entry.record.reason = reason;
        entry.cancel = None;
        entry.terminal_at_ms = Some(now);
        let record = entry.record.clone();
        self.commit_locked(inner, state.into(), &record);
        Some(record)
    }

    pub fn get(&self, run_id: &str, task_id: &str) -> Option<WorkflowTaskRecord> {
        self.inner
            .lock()
            .tasks
            .get(&(run_id.to_string(), task_id.to_string()))
            .map(|entry| entry.record.clone())
    }

    /// 某 run 下的全部任务（发布顺序）。
    pub fn for_run(&self, run_id: &str) -> Vec<WorkflowTaskRecord> {
        let inner = self.inner.lock();
        inner
            .order
            .iter()
            .filter(|(run, _)| run == run_id)
            .filter_map(|key| inner.tasks.get(key).map(|entry| entry.record.clone()))
            .collect()
    }

    /// 某 run 下未终态任务数（`maxAgents` 门限用；只计数，不 clone 记录）。
    ///
    /// 只返回计数：准入判定不需要任务正文。计数与 [`Self::publish`] 分属不同临界区，
    /// 结果仅作为发布前的准入提示；幂等与 kill 栅栏仍由发布路径在自身临界区裁决。
    pub fn active_count_for_run(&self, run_id: &str) -> usize {
        let inner = self.inner.lock();
        Self::count_states_locked(&inner, run_id, |state| !state.is_terminal())
    }

    /// 某 run 下执行中（`executing`）任务数（`maxConcurrency` 容量用；只计数，
    /// 不 clone 记录）。
    ///
    /// 与 [`Self::mark_executing`] 分属不同临界区：返回值用于容量门限，唯一 claim
    /// 仍由 `mark_executing` 的临界区裁决（计数不构成准入保证）。
    pub fn executing_count_for_run(&self, run_id: &str) -> usize {
        let inner = self.inner.lock();
        Self::count_states_locked(&inner, run_id, |state| {
            state == WorkflowTaskState::Executing
        })
    }

    /// 持锁内的按 run 状态计数：遍历 `order` 用 `tasks.get` 借用过滤，不 clone 记录。
    fn count_states_locked(
        inner: &TableInner,
        run_id: &str,
        matches: impl Fn(WorkflowTaskState) -> bool,
    ) -> usize {
        inner
            .order
            .iter()
            .filter(|(run, _)| run == run_id)
            .filter(|key| {
                inner
                    .tasks
                    .get(*key)
                    .is_some_and(|entry| matches(entry.record.state))
            })
            .count()
    }

    /// 某 run 下指定 agent 的任务（单 agent kill 路径）。
    pub fn for_agent(&self, run_id: &str, agent_id: u64) -> Vec<WorkflowTaskRecord> {
        self.for_run(run_id)
            .into_iter()
            .filter(|record| record.agent_id == agent_id)
            .collect()
    }

    /// session 级快照（发布顺序；读取前先按保留窗口淘汰过期终态任务）。
    pub fn snapshot(&self) -> Vec<WorkflowTaskRecord> {
        let mut inner = self.inner.lock();
        Self::trim_locked(&mut inner, now_ms());
        inner
            .order
            .iter()
            .filter_map(|key| inner.tasks.get(key).map(|entry| entry.record.clone()))
            .collect()
    }

    /// 全部未终态任务（消费者基线扫描）。
    pub fn active(&self) -> Vec<WorkflowTaskRecord> {
        self.baseline(None).active_tasks
    }

    /// 任务基线：未终态任务集合 + **同一临界区**读取的已提交上界
    /// （design §4.3 补读协议第 1/4 条：订阅确认后以全量扫描未终态任务建立基线；
    /// 游标早于保留边界时重新建立快照）。
    ///
    /// `active_tasks` 与 `head_sequence` 在一次表锁内读取：提交路径恒为
    /// 「持表锁 → 追加事件」，因此 `head_sequence` 水位内的状态变更都已在快照中
    /// 可见。事件日志保留窗口淘汰发布事件后，基线仍能完整找回活跃任务——
    /// 只读事件页会把「长期 published、发布事件已被淘汰」的任务永久漏掉
    /// （对抗审查 F3）。`run_id` 为 `None` 时是 session 级全量扫描。
    pub fn baseline(&self, run_id: Option<&str>) -> TaskBaseline {
        let mut inner = self.inner.lock();
        Self::trim_locked(&mut inner, now_ms());
        let active_tasks = inner
            .order
            .iter()
            .filter_map(|key| inner.tasks.get(key))
            .filter(|entry| !entry.record.state.is_terminal())
            .filter(|entry| run_id.is_none_or(|id| entry.record.run_id == id))
            .map(|entry| entry.record.clone())
            .collect();
        TaskBaseline {
            active_tasks,
            head_sequence: self.events.head_sequence(),
            truncated_before: self.events.truncated_before(),
        }
    }

    /// 保留窗口淘汰 + 硬上界淘汰（有界内存；只淘汰终态任务，活跃任务永不淘汰）。
    ///
    /// 运行期状态不需要 journal 耐久性：终态任务超过保留窗口即从快照表移除
    /// （事件日志是另一条有界流），与 progress store 的 run 清理窗口同量级。
    fn trim_locked(inner: &mut TableInner, now: i64) {
        let before = inner.tasks.len();
        if before == 0 {
            return;
        }
        inner.tasks.retain(|_, entry| {
            entry
                .terminal_at_ms
                .map(|at| now.saturating_sub(at) < TASK_RETENTION_MS)
                .unwrap_or(true)
        });
        if inner.tasks.len() > MAX_TASK_RECORDS || inner.tasks.len() != before {
            inner.order.retain(|key| inner.tasks.contains_key(key));
        }
        if inner.tasks.len() > MAX_TASK_RECORDS {
            let excess = inner.tasks.len() - MAX_TASK_RECORDS;
            let victims: Vec<(String, String)> = inner
                .order
                .iter()
                .filter(|key| {
                    inner
                        .tasks
                        .get(*key)
                        .is_some_and(|entry| entry.record.state.is_terminal())
                })
                .take(excess)
                .cloned()
                .collect();
            for key in victims {
                inner.tasks.remove(&key);
            }
            inner.order.retain(|key| inner.tasks.contains_key(key));
        }
    }

    /// 持锁内的事件提交：追加任务事件 → 发订阅唤醒（先可见后通知）。
    ///
    /// 与状态变更在同一临界区完成，保证事件顺序 == 状态提交顺序（无锁外反序窗口）。
    /// 调用方必须已持有 `inner`（参数仅用于声明该前置条件）；锁序恒为
    /// 「任务表 → 事件日志」，无反向路径。
    fn commit_locked(
        &self,
        _inner: &mut TableInner,
        kind: WorkflowTaskEventKind,
        record: &WorkflowTaskRecord,
    ) {
        self.events.append_with(|sequence| WorkflowTaskEvent {
            sequence,
            event_id: format!(
                "{}:{}:task.{}",
                record.run_id,
                record.task_id,
                kind.as_str()
            ),
            run_id: record.run_id.clone(),
            task_id: record.task_id.clone(),
            agent_id: record.agent_id,
            kind,
            at_ms: now_ms(),
            task: record.clone(),
        });
        let _ = self.updates.send(WorkflowResourceUpdate::Tasks);
        let _ = self.updates.send(WorkflowResourceUpdate::RunTasks {
            run_id: record.run_id.clone(),
        });
        // 先可见后通知：终态提交已落事件与订阅唤醒，最后通知投递面（`agent/result`）。
        if record.state.is_terminal() {
            self.notify_terminal(record);
        }
    }
}

/// 结果摘要（只用于日志，不承载语义）：`ok` / `skipped` / `dead`。
fn result_summary(result: Option<&AgentRunResult>) -> Option<&'static str> {
    match result? {
        AgentRunResult::Ok { .. } => Some("ok"),
        AgentRunResult::Skipped => Some("skipped"),
        AgentRunResult::Dead { .. } => Some("dead"),
    }
}

/// wall clock 毫秒（事件时间戳，供展示与排序参考；不作为生命周期判定依据）。
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
#[path = "tasks_test.rs"]
mod tests;
