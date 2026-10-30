use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use peri_acp_types::tasks::{
    BgRegistryEvent, BgTaskKind, ExternalCancelFn, ExternalNotifyFn, TaskChange, TaskRecord,
    TaskRegistrationTicket, TaskSnapshot,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::agent::events::BackgroundTaskResult;

use super::agent_inbox::{BackgroundAgentInbox, QueuedSubagentMessage, SubagentMessageError};

#[path = "projection.rs"]
mod projection;
#[path = "registration.rs"]
mod registration;
#[path = "settlement.rs"]
mod settlement;
use projection::TaskProjection;
use registration::RegistrationReceipt;
#[path = "cancel.rs"]
mod cancel;

#[cfg(test)]
#[path = "projection_retention_test.rs"]
mod projection_retention_tests;

/// 后台任务注册表错误（结构化，取代 String 错误）
///
/// 实现 `std::error::Error`，调用方可通过 `?` 自动转 `Box<dyn Error>` /
/// `anyhow::Error`。
#[derive(Debug, Error)]
pub enum BackgroundRegistryError {
    #[error("Session execution scope is closing")]
    Closing,
    #[error("Maximum {0} concurrent background tasks reached")]
    ConcurrentLimit(usize),
    #[error("Task {0} not found")]
    TaskNotFound(String),
    #[error("Task {0} is completing")]
    TaskCompleting(String),
    #[error("Task {0} cannot be cancelled: kill handle unavailable")]
    KillUnavailable(String),
    #[error("Task {0} already exists")]
    DuplicateTask(String),
    #[error("Task {0} registration ticket does not belong to this execution")]
    InvalidRegistration(String),
    #[error("Task {0} has already started execution")]
    TaskAlreadyStarted(String),
    #[error("External task {0} source identity cannot change")]
    ExternalIdentityConflict(String),
    #[error("Task {task_id} completion delivery failed: {reason}")]
    DeliveryFailed { task_id: String, reason: String },
    #[error("Task {0} requires asynchronous cancellation")]
    ExternalCancelRequiresAsync(String),
    #[error("External owner rejected cancellation for task {0}")]
    ExternalCancelFailed(String),
    #[error("External task has an invalid creation timestamp")]
    InvalidStartedAt,
    #[error(
        "External task {task_id} initiator conflict: recorded {recorded}, requested {requested}"
    )]
    ExternalInitiatorConflict {
        task_id: String,
        recorded: String,
        requested: String,
    },
    #[error("External task {0} is Unroutable: initiator is unknown")]
    ExternalUnroutable(String),
    #[error("Kind concurrent limit reached: {kind} ({current}/{limit})")]
    KindConcurrentLimit {
        kind: String,
        current: usize,
        limit: usize,
    },
}

pub enum BgCancelHandle {
    /// bg agent：取消 tokio task。
    /// 持 `JoinHandle`（而非 `AbortHandle`）——取消时先 `token.cancel()` 让任务
    /// 优雅退出，再 await JoinHandle 等待其走完收尾，超时才 abort。
    Abort(tokio::task::JoinHandle<()>),
    /// workflow：kill 闭包——转发到 `WorkflowTaskRegistry::kill`（真正的 kill_tx 在其内部）。
    /// `None` 表示 kill 通道不可用（如 spawn 失败），此时 `cancel()` 返回明确错误
    /// 而非假装成功（issue 2026-08-05：Workflow 取消无效）。
    Kill(Option<Box<dyn FnOnce() + Send + Sync>>),
    External {
        cancel: ExternalCancelFn,
        on_terminal: ExternalNotifyFn,
    },
}

impl std::fmt::Debug for BgCancelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BgCancelHandle::Abort(_) => f.write_str("Abort(_)"),
            BgCancelHandle::Kill(_) => f.write_str("Kill(_)"),
            BgCancelHandle::External { .. } => f.write_str("External { .. }"),
        }
    }
}

/// 后台任务信息（注册表条目）
pub struct BackgroundTask {
    pub id: String,
    pub agent_name: String,
    pub prompt_summary: String,
    pub status: BackgroundTaskStatus,
    pub started_at: std::time::Instant,
    /// 任务创建时间（chrono UTC），用于 list_tasks_full().started_at 返回真实时间
    pub chrono_started_at: chrono::DateTime<chrono::Utc>,
    /// 任务类型
    pub kind: BgTaskKind,
    /// 按 kind 分发的取消句柄
    pub cancel_handle: BgCancelHandle,
    /// 取消令牌（仅 Agent 类任务）：cancel() 时先 `token.cancel()` 让工具层取消链
    /// 生效（run_react_loop 的 await 点响应后走完整收尾），超时再 abort 兜底。
    /// Shell/Workflow 类任务为 None（取消走 Pid/Kill 句柄）。
    pub cancel_token: Option<CancellationToken>,
    /// OS 进程 PID（仅 bg shell 有效）
    pub pid: Option<u32>,
    /// 输出预览（completed 时写入，最多 500 字符）
    pub output_preview: Option<String>,
    /// Present only for live background sub-agents; revoked before terminal events.
    pub agent_inbox: Option<Arc<BackgroundAgentInbox>>,
    /// 投递归属（直接发起会话）。`None` = 本地 owner 任务或测试构造。
    pub initiator_session_id: Option<String>,
    /// 执行 scope owner（root 会话）：有界等待到期时交接记录里的 scope owner。
    pub owner_session_id: Option<String>,
    /// Owner 身份（MCP 实例身份等），交接记录用。
    pub owner_identity: Option<String>,
}

/// 后台任务状态
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Running,
    /// Completion has been claimed; callbacks run outside the registry lock.
    Completing,
    Completed,
    Failed,
}

fn is_active_status(status: &BackgroundTaskStatus) -> bool {
    matches!(
        status,
        BackgroundTaskStatus::Running | BackgroundTaskStatus::Completing
    )
}

/// 后台任务信息 DTO（序列化用）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BgTaskInfo {
    pub task_id: String,
    pub kind: BgTaskKind,
    pub summary: String,
    pub status: BackgroundTaskStatus,
    pub started_at: String,
    pub duration_ms: u64,
    pub pid: Option<u32>,
    pub output_preview: Option<String>,
}

/// 后台任务注册中心
pub struct BackgroundTaskRegistry {
    tasks: parking_lot::Mutex<HashMap<String, BackgroundTask>>,
    registration_owner: uuid::Uuid,
    registrations: parking_lot::Mutex<HashMap<String, Arc<RegistrationReceipt>>>,
    projection: parking_lot::Mutex<TaskProjection>,
    changes: tokio::sync::broadcast::Sender<TaskChange>,
    /// Derived wake signal for consumers waiting on task lifecycle changes.
    /// The registry remains the sole source of task state; this version is only
    /// a retained notification channel for re-checking that state.
    activity_version: tokio::sync::watch::Sender<u64>,
    event_sender: parking_lot::RwLock<Option<tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>>>,
    session_id: parking_lot::RwLock<String>,
    pub(super) scope: Arc<super::scope::ExecutionScope>,
    unsettled_external: parking_lot::Mutex<HashSet<String>>,
    cancelled_tasks_waiting_cleanup: parking_lot::Mutex<HashMap<String, BgTaskKind>>,
    pending_deliveries: parking_lot::Mutex<HashMap<String, settlement::PendingTaskDelivery>>,
    delivery_worker_running: parking_lot::Mutex<bool>,
    settlements_in_flight: std::sync::atomic::AtomicUsize,
}

enum TaskRegistration {
    Created,
    PendingStart,
    TerminalRecovery,
}

impl Default for BackgroundTaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundTaskRegistry {
    /// 后台 shell 并发上限。多 Agent 会话共享同一实例，5 槽易被长驻任务占满；
    /// 2026-10-19 按容量决策调整为 20。
    pub const SHELL_LIMIT: usize = 20;
    pub const WORKFLOW_LIMIT: usize = 3;

    /// per-kind 并发上限：`None` = 不限额。
    ///
    /// Agent 类（后台 sub-agent）不设上限——用户要求放开并行委派；取消其上限
    /// 不改变 Shell / Workflow 的独立上限语义。
    fn kind_limit(kind: BgTaskKind) -> Option<usize> {
        match kind {
            BgTaskKind::Shell => Some(Self::SHELL_LIMIT),
            BgTaskKind::Workflow => Some(Self::WORKFLOW_LIMIT),
            BgTaskKind::Agent => None,
            BgTaskKind::Mcp => None,
        }
    }

    pub fn new() -> Self {
        let (activity_version, _) = tokio::sync::watch::channel(0_u64);
        let (changes, _) = tokio::sync::broadcast::channel(256);
        Self {
            tasks: parking_lot::Mutex::new(HashMap::new()),
            registration_owner: uuid::Uuid::now_v7(),
            registrations: parking_lot::Mutex::new(HashMap::new()),
            projection: parking_lot::Mutex::new(TaskProjection::default()),
            changes,
            activity_version,
            event_sender: parking_lot::RwLock::new(None),
            session_id: parking_lot::RwLock::new(String::new()),
            scope: super::scope::ExecutionScope::new(),
            unsettled_external: parking_lot::Mutex::new(HashSet::new()),
            cancelled_tasks_waiting_cleanup: parking_lot::Mutex::new(HashMap::new()),
            pending_deliveries: parking_lot::Mutex::new(HashMap::new()),
            delivery_worker_running: parking_lot::Mutex::new(false),
            settlements_in_flight: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 取任务投影快照。
    ///
    /// `revision` 与记录副本在同一锁区取出，保证两者来自同一时点；排序在锁外
    /// 进行，避免持锁期间做 O(n log n) 排序阻塞注册 / 完成路径。
    pub fn snapshot(&self) -> TaskSnapshot {
        let (revision, mut tasks): (u64, Vec<TaskRecord>) = {
            let projection = self.projection.lock();
            (
                projection.revision,
                projection.records.values().cloned().collect(),
            )
        };
        tasks.sort_by(|a, b| {
            a.started_at
                .cmp(&b.started_at)
                .then_with(|| a.task_id.cmp(&b.task_id))
        });
        TaskSnapshot { revision, tasks }
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<TaskChange> {
        self.changes.subscribe()
    }

    /// 设置 ACP 事件推送通道（由 executor 在 run_session_loop 调用）
    pub fn set_event_sender(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<BgRegistryEvent>,
        session_id: String,
    ) {
        *self.event_sender.write() = Some(sender);
        *self.session_id.write() = session_id;
    }

    /// 清除 ACP 事件推送通道（session 结束时调用）
    pub fn clear_event_sender(&self) {
        *self.event_sender.write() = None;
        self.session_id.write().clear();
    }

    /// 当前运行中的任务数
    pub fn active_count(&self) -> usize {
        self.tasks
            .lock()
            .values()
            .filter(|t| is_active_status(&t.status))
            .count()
    }

    /// Subscribe to retained notifications of task lifecycle changes.
    ///
    /// The returned version is only a wake signal. Callers must re-check
    /// [`Self::active_count`] and the queue after every notification.
    pub fn subscribe_activity(&self) -> tokio::sync::watch::Receiver<u64> {
        self.activity_version.subscribe()
    }

    /// Claim a single completion before running any external callback.
    ///
    /// The claim is linearized with cancellation while holding the task lock,
    /// but no callback is run under that lock. A claimed task remains active
    /// until [`Self::complete`] settles and removes it.
    pub(super) fn claim_completion(&self, ticket: &TaskRegistrationTicket) -> bool {
        let mut tasks = self.tasks.lock();
        if self.validate_ticket(ticket).is_err() || ticket.is_settled() {
            return false;
        }
        let task_id = ticket.task_id();
        let Some(task) = tasks.get_mut(task_id) else {
            return false;
        };
        if !matches!(task.status, BackgroundTaskStatus::Running) {
            return false;
        }
        task.status = BackgroundTaskStatus::Completing;
        true
    }

    pub(super) fn reset_completion_claim(&self, ticket: &TaskRegistrationTicket) {
        let mut tasks = self.tasks.lock();
        if self.validate_ticket(ticket).is_err() || ticket.is_settled() {
            return;
        }
        if let Some(task) = tasks.get_mut(ticket.task_id()) {
            if matches!(task.status, BackgroundTaskStatus::Completing) {
                task.status = BackgroundTaskStatus::Running;
            }
        }
    }

    pub(super) fn external_notify(&self, task_id: &str) -> Option<ExternalNotifyFn> {
        let tasks = self.tasks.lock();
        match &tasks.get(task_id)?.cancel_handle {
            BgCancelHandle::External { on_terminal, .. } => Some(Arc::clone(on_terminal)),
            _ => None,
        }
    }

    pub(super) fn external_callback_refresh_allowed(
        &self,
        task_id: &str,
        requested: Option<&str>,
    ) -> Result<bool, BackgroundRegistryError> {
        let tasks = self.tasks.lock();
        let projection = self.projection.lock();
        let recorded = projection
            .records
            .get(task_id)
            .and_then(|record| record.initiator_session_id.as_deref());
        match (recorded, requested) {
            (Some(recorded), Some(requested)) if recorded != requested => {
                Err(BackgroundRegistryError::ExternalInitiatorConflict {
                    task_id: task_id.into(),
                    recorded: recorded.into(),
                    requested: requested.into(),
                })
            }
            (Some(_), None) => Ok(false),
            (None, Some(_))
                if tasks.get(task_id).is_some_and(|task| {
                    matches!(task.status, BackgroundTaskStatus::Completing)
                }) =>
            {
                Err(BackgroundRegistryError::TaskCompleting(task_id.into()))
            }
            _ => Ok(true),
        }
    }

    pub(super) fn ensure_external_routable(
        &self,
        task_id: &str,
    ) -> Result<(), BackgroundRegistryError> {
        if self
            .projection
            .lock()
            .records
            .get(task_id)
            .and_then(|record| record.initiator_session_id.as_ref())
            .is_none()
        {
            return Err(BackgroundRegistryError::ExternalUnroutable(task_id.into()));
        }
        Ok(())
    }

    pub(super) fn refresh_external_callbacks(
        &self,
        task_id: &str,
        cancel: ExternalCancelFn,
        on_terminal: ExternalNotifyFn,
        initiator_session_id: Option<String>,
    ) {
        let mut tasks = self.tasks.lock();
        if let Some(task) = tasks.get_mut(task_id) {
            if matches!(task.status, BackgroundTaskStatus::Running)
                && matches!(task.cancel_handle, BgCancelHandle::External { .. })
            {
                task.cancel_handle = BgCancelHandle::External {
                    cancel,
                    on_terminal,
                };
                if task.initiator_session_id.is_none() && initiator_session_id.is_some() {
                    task.initiator_session_id = initiator_session_id.clone();
                    if let Some(receipt) = self.registrations.lock().get(task_id) {
                        *receipt.initiator_session_id.write() = initiator_session_id.clone();
                    }
                    let mut projection = self.projection.lock();
                    if let Some(record) = projection.records.get_mut(task_id) {
                        record.initiator_session_id = initiator_session_id;
                        let status = record.status.clone();
                        self.push_event(
                            &mut projection,
                            BgRegistryEvent::Updated {
                                task_id: task_id.into(),
                                status,
                            },
                        );
                    }
                }
            }
        }
    }

    pub(super) fn claim_cancelled_cleanup(
        &self,
        ticket: &TaskRegistrationTicket,
    ) -> Option<BgTaskKind> {
        let _tasks = self.tasks.lock();
        if self.validate_ticket(ticket).is_err() || ticket.is_settled() {
            return None;
        }
        self.cancelled_tasks_waiting_cleanup
            .lock()
            .remove(ticket.task_id())
    }

    pub(super) fn has_pending_delivery(&self, task_id: &str) -> bool {
        self.pending_deliveries.lock().contains_key(task_id)
    }

    pub(super) fn has_pending_deliveries(&self) -> bool {
        !self.pending_deliveries.lock().is_empty()
    }

    /// Unsettled tasks for the bounded-wait handoff record: public identity plus
    /// the scope owner that will reconcile them. Empty = nothing pending.
    pub(super) fn pending_handoff_tasks(&self) -> Vec<super::handoff::PendingHandoffTask> {
        let tasks = self.tasks.lock();
        let pending_deliveries = self.pending_deliveries.lock();
        let mut pending: Vec<_> = tasks
            .values()
            .filter(|task| {
                is_active_status(&task.status) || pending_deliveries.contains_key(&task.id)
            })
            .map(|task| super::handoff::PendingHandoffTask {
                task_id: task.id.clone(),
                kind: task.kind,
                owner_session_id: task.owner_session_id.clone(),
                owner_identity: task.owner_identity.clone(),
            })
            .collect();
        drop(pending_deliveries);
        for (task_id, receipt) in self.registrations.lock().iter() {
            if !tasks.contains_key(task_id) {
                pending.push(super::handoff::PendingHandoffTask {
                    task_id: task_id.clone(),
                    kind: receipt.kind,
                    owner_session_id: receipt.owner_session_id.clone(),
                    owner_identity: receipt.owner_identity.clone(),
                });
            }
        }
        pending.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        pending
    }

    pub(super) fn external_cancel(&self, task_id: &str) -> Option<ExternalCancelFn> {
        let tasks = self.tasks.lock();
        let task = tasks.get(task_id)?;
        if !matches!(task.status, BackgroundTaskStatus::Running) {
            return None;
        }
        match &task.cancel_handle {
            BgCancelHandle::External { cancel, .. } => Some(Arc::clone(cancel)),
            _ => None,
        }
    }

    pub(super) fn mark_external_status(&self, task_id: &str, status: &str) -> bool {
        let tasks = self.tasks.lock();
        let Some(task) = tasks.get(task_id) else {
            return false;
        };
        if !matches!(task.cancel_handle, BgCancelHandle::External { .. })
            || !is_active_status(&task.status)
        {
            return false;
        }
        let mut projection = self.projection.lock();
        let changed = {
            if let Some(record) = projection.records.get_mut(task_id) {
                if record.status == status {
                    false
                } else {
                    record.status = status.to_owned();
                    true
                }
            } else {
                false
            }
        };
        if changed {
            self.push_event(
                &mut projection,
                BgRegistryEvent::Updated {
                    task_id: task_id.to_owned(),
                    status: status.to_owned(),
                },
            );
        }
        changed
    }

    /// 按类型统计运行中任务数
    pub fn count_by_kind(&self, kind: BgTaskKind) -> usize {
        self.tasks
            .lock()
            .values()
            .filter(|t| is_active_status(&t.status) && t.kind == kind)
            .count()
    }

    pub(super) fn send_subagent_message(
        &self,
        thread_id: &str,
        prompt: Option<&str>,
    ) -> Result<Option<QueuedSubagentMessage>, SubagentMessageError> {
        let _admission = self
            .scope
            .admit()
            .map_err(|_| SubagentMessageError::Closed)?;
        let tasks = self.tasks.lock();
        let target = tasks.values().find_map(|task| {
            if task.kind != BgTaskKind::Agent
                || !matches!(task.status, BackgroundTaskStatus::Running)
            {
                return None;
            }
            task.agent_inbox
                .as_ref()
                .filter(|inbox| inbox.thread_id == thread_id)
                .map(|inbox| (task, inbox))
        });
        target
            .map(|(task, inbox)| inbox.send(&task.id, prompt))
            .transpose()
    }

    /// 按类型注册新任务（Shell / Workflow 有独立上限；Agent 不限额）
    pub fn register_with_kind(
        &self,
        task: BackgroundTask,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        let _admission = self
            .scope
            .admit()
            .map_err(|_| BackgroundRegistryError::Closing)?;
        self.register_external_admitted(task)
    }

    pub(super) fn register_pending_start(
        &self,
        task: BackgroundTask,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        let _admission = self
            .scope
            .admit()
            .map_err(|_| BackgroundRegistryError::Closing)?;
        let limit = Self::kind_limit(task.kind);
        self.register_task(task, limit, TaskRegistration::PendingStart, None)
    }

    /// Caller holds scope admission across external process creation and registration.
    pub fn register_external_admitted(
        &self,
        task: BackgroundTask,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        let limit = Self::kind_limit(task.kind);
        self.register_task(task, limit, TaskRegistration::Created, None)
    }

    pub(super) fn register_external_task(
        &self,
        task: BackgroundTask,
        source_identity: String,
        terminal: bool,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        // Existing source terminals may still need delivery while execution is
        // closing; this admission creates no new execution or external resource.
        let _admission = if terminal {
            None
        } else {
            Some(
                self.scope
                    .admit()
                    .map_err(|_| BackgroundRegistryError::Closing)?,
            )
        };
        let (limit, registration) = if terminal {
            (None, TaskRegistration::TerminalRecovery)
        } else {
            (Self::kind_limit(task.kind), TaskRegistration::Created)
        };
        self.register_task(task, limit, registration, Some(source_identity))
    }

    fn register_task(
        &self,
        task: BackgroundTask,
        limit: Option<usize>,
        registration: TaskRegistration,
        source_identity: Option<String>,
    ) -> Result<TaskRegistrationTicket, BackgroundRegistryError> {
        let kind = task.kind;
        let task_id = task.id.clone();
        let summary =
            super::shell::truncate_bytes(&task.prompt_summary, projection::SUMMARY_BYTE_LIMIT);
        let status = match registration {
            TaskRegistration::Created | TaskRegistration::PendingStart => "running",
            TaskRegistration::TerminalRecovery => "pending_delivery",
        };

        let mut tasks = self.tasks.lock();
        if let Some(limit) = limit {
            let current = tasks
                .values()
                .filter(|t| is_active_status(&t.status) && t.kind == kind)
                .count();
            if current >= limit {
                let kind_str = match kind {
                    BgTaskKind::Shell => "shell",
                    BgTaskKind::Agent => "agent",
                    BgTaskKind::Workflow => "workflow",
                    BgTaskKind::Mcp => "mcp",
                };
                return Err(BackgroundRegistryError::KindConcurrentLimit {
                    kind: kind_str.to_string(),
                    current,
                    limit,
                });
            }
        }

        if tasks.contains_key(&task.id) || self.registrations.lock().contains_key(&task.id) {
            return Err(BackgroundRegistryError::DuplicateTask(task.id));
        }
        if let Some(source) = source_identity.as_deref() {
            if let Some(existing) = self
                .registrations
                .lock()
                .values()
                .find(|receipt| receipt.ticket().source_identity() == Some(source))
            {
                return Err(BackgroundRegistryError::DuplicateTask(
                    existing.ticket().task_id().into(),
                ));
            }
        }
        let receipt = RegistrationReceipt::new(
            task.id.clone(),
            self.registration_owner,
            source_identity,
            &task,
            !matches!(registration, TaskRegistration::PendingStart),
        );
        let ticket = receipt.ticket();
        self.registrations.lock().insert(task.id.clone(), receipt);
        if !matches!(&task.cancel_handle, BgCancelHandle::Abort(_)) {
            self.unsettled_external.lock().insert(task.id.clone());
        }
        let mut projection = self.projection.lock();
        projection.insert(TaskRecord {
            task_id: task.id.clone(),
            kind,
            summary: summary.clone(),
            started_at: task.chrono_started_at.to_rfc3339(),
            status: status.into(),
            duration_ms: 0,
            output_preview: None,
            initiator_session_id: task.initiator_session_id.clone(),
            // 线程身份的事实源是注册时的 live inbox；取快照进投影，终态回收
            // inbox（complete 时 close、active 条目 retain 移除）后机器消费者
            // 仍能拿真实身份，不必按到达顺序猜测或由 `bg-` 前缀推导。
            child_thread_id: task
                .agent_inbox
                .as_ref()
                .map(|inbox| inbox.thread_id.clone()),
        });
        tasks.insert(task.id.clone(), task);
        let event = match registration {
            TaskRegistration::Created | TaskRegistration::PendingStart => {
                BgRegistryEvent::Started {
                    task_id,
                    kind,
                    summary,
                    started_at: peri_time::now_utc_rfc3339(),
                }
            }
            TaskRegistration::TerminalRecovery => BgRegistryEvent::Updated {
                task_id,
                status: status.into(),
            },
        };
        self.push_event(&mut projection, event);
        drop(projection);
        drop(tasks);
        self.notify_activity_change();

        Ok(ticket)
    }

    /// 任务完成时调用：更新状态 + 推送通知。
    ///
    /// 返回 `true` 表示条目存在且已处理；`false` 表示任务已不在 registry
    /// （如已被 cancel 移除后自然完成），此时不推送 Completed 事件——否则会
    /// 产生幽灵完成事件（issue 2026-08-05：kill 后仍推 bg-task-completed）。
    pub fn complete(&self, ticket: &TaskRegistrationTicket, result: BackgroundTaskResult) -> bool {
        if self.validate_ticket(ticket).is_err() || ticket.is_settled() {
            return false;
        }
        let task_id = ticket.task_id();
        tracing::info!(
            task_id = %task_id,
            agent_name = %result.agent_name,
            success = result.success,
            output_len = result.output.len(),
            "[bg-diag] registry.complete() called"
        );
        let duration_ms = result.duration_ms;
        let success = result.success;
        let output_preview: String = result.output.chars().take(500).collect();

        // 持锁：更新状态 + 清理所有已结算任务，防止 JoinHandle 长期驻留内存
        let mut tasks = self.tasks.lock();
        if self.validate_ticket(ticket).is_err() || ticket.is_settled() {
            return false;
        }
        let kind = tasks.get(task_id).map(|task| task.kind);
        let existed = tasks
            .get(task_id)
            .map(|task| is_active_status(&task.status))
            .unwrap_or(false);
        if existed {
            let task = tasks
                .get_mut(task_id)
                .expect("active task must remain present while registry lock is held");
            let external_terminal = matches!(task.cancel_handle, BgCancelHandle::External { .. });
            if let Some(inbox) = &task.agent_inbox {
                inbox.close();
            }
            task.status = if result.success {
                BackgroundTaskStatus::Completed
            } else {
                BackgroundTaskStatus::Failed
            };
            task.output_preview = Some(output_preview.clone());
            let mut projection = self.projection.lock();
            if let Some(record) = projection.records.get_mut(task_id) {
                record.status = if success { "completed" } else { "failed" }.into();
                record.duration_ms = duration_ms;
                record.output_preview = Some(output_preview.clone());
            }
            // Keep the projection lock until the terminal revision is published.
            tasks.retain(|_, t| is_active_status(&t.status));
            if external_terminal {
                self.unsettled_external.lock().remove(task_id);
            }
            self.mark_registration_settled(ticket);
            if self.release_settled_registration(task_id) {
                projection.retire(task_id);
            }
            self.push_event(
                &mut projection,
                BgRegistryEvent::Completed {
                    task_id: task_id.to_string(),
                    kind,
                    success,
                    output_preview,
                    duration_ms,
                    result,
                },
            );
            drop(projection);
            drop(tasks);
            self.notify_activity_change();
            return true;
        }
        tasks.retain(|_, t| is_active_status(&t.status));

        drop(tasks);
        // Execution-only producers still own their cooperative cancellation
        // result. Settle that responsibility without a second UI terminal;
        // real external cleanup remains independently pinned until confirmed.
        if self.claim_cancelled_cleanup(ticket).is_some() {
            self.mark_registration_settled(ticket);
            self.retire_settled_projection(ticket);
            self.notify_activity_change();
            return false;
        }
        warn!(
            task_id = %task_id,
            agent_name = %result.agent_name,
            success,
            "background registry: completion has no active or cancelled registration; \
             Completed event suppressed"
        );
        false
    }

    /// 获取所有任务状态（UI 使用）
    pub fn list_tasks(&self) -> Vec<(String, BackgroundTaskStatus, String)> {
        self.tasks
            .lock()
            .values()
            .map(|t| (t.id.clone(), t.status.clone(), t.prompt_summary.clone()))
            .collect()
    }

    /// 获取完整任务信息（供 ACP Snapshot / TUI 面板使用）
    pub fn list_tasks_full(&self) -> Vec<BgTaskInfo> {
        self.tasks
            .lock()
            .values()
            .map(|t| BgTaskInfo {
                task_id: t.id.clone(),
                kind: t.kind,
                summary: t.prompt_summary.clone(),
                status: t.status.clone(),
                started_at: t.chrono_started_at.to_rfc3339(),
                duration_ms: t.started_at.elapsed().as_millis() as u64,
                pid: t.pid,
                output_preview: t.output_preview.clone(),
            })
            .collect()
    }

    /// 清理已完成的任务
    pub fn cleanup_completed(&self) {
        self.tasks.lock().retain(|_, t| is_active_status(&t.status));
    }

    pub(super) fn external_settled(&self) -> bool {
        // A cancelled producer may be between claiming its result and committing
        // acceptance. The receipt remains its owner across that transition.
        self.registrations.lock().is_empty()
            && self.unsettled_external.lock().is_empty()
            && self.cancelled_tasks_waiting_cleanup.lock().is_empty()
            && self.pending_deliveries.lock().is_empty()
            && self
                .settlements_in_flight
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
    }

    pub(super) fn has_unsettled_mcp(&self) -> bool {
        self.tasks.lock().values().any(|task| {
            matches!(task.cancel_handle, BgCancelHandle::External { .. })
                && is_active_status(&task.status)
        })
    }

    pub(super) fn local_task_ids(&self) -> Vec<String> {
        self.tasks
            .lock()
            .values()
            .filter(|task| !matches!(task.cancel_handle, BgCancelHandle::External { .. }))
            .map(|task| task.id.clone())
            .collect()
    }

    pub(super) fn external_task_ids(&self) -> Vec<String> {
        self.tasks
            .lock()
            .values()
            .filter(|task| {
                matches!(task.cancel_handle, BgCancelHandle::External { .. })
                    && is_active_status(&task.status)
            })
            .map(|task| task.id.clone())
            .collect()
    }

    pub fn spawn_execution_cleanup(
        &self,
        task: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        self.scope.spawn_admitted(task)
    }

    pub fn confirm_external_stopped(&self, ticket: &TaskRegistrationTicket) {
        let _tasks = self.tasks.lock();
        if self.validate_ticket(ticket).is_err() {
            return;
        }
        // A retired ticket cannot clear cleanup evidence for a reused ID.
        let current = self
            .registrations
            .lock()
            .get(ticket.task_id())
            .map(|receipt| receipt.ticket());
        if current
            .as_ref()
            .is_some_and(|current| ticket.same_registration(current))
        {
            let changed = self.unsettled_external.lock().remove(ticket.task_id());
            drop(_tasks);
            self.retire_settled_projection(ticket);
            if changed {
                self.notify_activity_change();
            }
        }
    }
}

impl BackgroundTaskRegistry {
    pub(super) fn notify_activity_change(&self) {
        self.activity_version
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    /// 推送 registry 事件到 ACP 层（非阻塞，channel 满时静默丢弃）
    fn push_event(&self, projection: &mut TaskProjection, event: BgRegistryEvent) {
        projection.revision = projection.revision.wrapping_add(1);
        let revision = projection.revision;
        let _ = self.changes.send(TaskChange {
            revision,
            event: event.clone(),
        });
        if let Some(sender) = self.event_sender.read().as_ref() {
            if sender.send(event).is_err() {
                warn!("background registry: event channel closed");
            }
        }
    }
}
