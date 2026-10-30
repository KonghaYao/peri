//! WorkflowTaskRegistry —— 管理活跃 workflow run，并发限制 + 完成通知。

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use peri_acp_types::tasks::TaskRecord;
use thiserror::Error;

// 3.0 批 2 波 1：协议类型迁入契约层 `peri_acp_types::workflow`
// （`WorkflowRunStatus` / `WorkflowTaskResult`）；本模块保留 re-export 保兼容。
pub use peri_acp_types::workflow::{WorkflowRunStatus, WorkflowTaskResult};

/// Registry 层错误。
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("Maximum {0} concurrent workflows reached")]
    ConcurrentLimit(usize),
    #[error("Workflow {0} not found")]
    NotFound(String),
    #[error("Workflow {0} is already registered")]
    AlreadyRegistered(String),
}

#[derive(Debug)]
struct RunReceipt {
    run_id: String,
    observed: AtomicBool,
    changed: tokio::sync::Notify,
}

/// A producer owns its observation evidence even after the MCP consumer ACKs
/// the terminal result and the registry releases the run. Never reconstructed
/// from a run ID or retained in a permanent acknowledged-ID directory.
#[derive(Clone, Debug)]
pub struct WorkflowRunTicket(Arc<RunReceipt>);

impl PartialEq for WorkflowRunTicket {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for WorkflowRunTicket {}

impl WorkflowRunTicket {
    pub fn run_id(&self) -> &str {
        &self.0.run_id
    }

    fn acknowledge_observation(&self) {
        self.0.observed.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }

    pub async fn wait_for_observation(&self) {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.0.observed.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }
}

struct PendingCompletion {
    result: WorkflowTaskResult,
    task_record: TaskRecord,
}

fn release_excess_capacity<V>(map: &mut HashMap<String, V>) {
    let target = map.len().max(64);
    if map.capacity() > target.saturating_mul(2) {
        map.shrink_to(target);
    }
}

/// 单个 workflow run 记录。
pub struct WorkflowRun {
    pub run_id: String,
    pub workflow_name: String,
    pub script_preview: String,
    pub status: WorkflowRunStatus,
    pub started_at: std::time::Instant,
    pub child_handle: Option<tokio::task::JoinHandle<()>>,
    pub kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

/// 管理活跃 workflow run，并发限制 + 完成通知。
pub struct WorkflowTaskRegistry {
    runs: parking_lot::Mutex<HashMap<String, WorkflowRun>>,
    notification_tx: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
    max_concurrent: usize,
    registrations: parking_lot::Mutex<HashMap<String, WorkflowRunTicket>>,
    completions: parking_lot::Mutex<HashMap<String, PendingCompletion>>,
}

impl WorkflowTaskRegistry {
    /// 创建 registry，默认最大并发 3。
    pub fn new(notification_tx: tokio::sync::broadcast::Sender<WorkflowTaskResult>) -> Self {
        Self {
            runs: parking_lot::Mutex::new(HashMap::new()),
            notification_tx,
            max_concurrent: 3,
            registrations: parking_lot::Mutex::new(HashMap::new()),
            completions: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// 设置最大并发数（builder）。
    pub fn with_max_concurrent(mut self, max: usize) -> Self {
        self.max_concurrent = max;
        self
    }

    /// 获取 notification sender 的克隆。
    pub fn notification_tx(&self) -> tokio::sync::broadcast::Sender<WorkflowTaskResult> {
        self.notification_tx.clone()
    }

    /// 当前正在运行的 workflow 数量。
    pub fn active_count(&self) -> usize {
        let runs = self.runs.lock();
        runs.values()
            .filter(|r| r.status == WorkflowRunStatus::Running)
            .count()
    }

    pub fn reserve(&self, run: WorkflowRun) -> Result<WorkflowRunTicket, RegistryError> {
        self.register(run)
    }

    /// 注册一个新的 workflow run，超出并发限制返回错误。
    pub fn register(&self, run: WorkflowRun) -> Result<WorkflowRunTicket, RegistryError> {
        let mut runs = self.runs.lock();
        let mut registrations = self.registrations.lock();
        if registrations.contains_key(&run.run_id) {
            return Err(RegistryError::AlreadyRegistered(run.run_id));
        }
        let active = runs
            .values()
            .filter(|r| r.status == WorkflowRunStatus::Running)
            .count();
        if active >= self.max_concurrent {
            return Err(RegistryError::ConcurrentLimit(self.max_concurrent));
        }
        let ticket = WorkflowRunTicket(Arc::new(RunReceipt {
            run_id: run.run_id.clone(),
            observed: AtomicBool::new(false),
            changed: tokio::sync::Notify::new(),
        }));
        registrations.insert(run.run_id.clone(), ticket.clone());
        runs.insert(run.run_id.clone(), run);
        Ok(ticket)
    }

    pub fn attach_child(&self, run_id: &str, child_handle: tokio::task::JoinHandle<()>) {
        if let Some(run) = self.runs.lock().get_mut(run_id) {
            run.child_handle = Some(child_handle);
        } else {
            // Cancellation can remove the reservation before this handle arrives.
            // Its kill signal still owns cleanup; aborting would skip that proof.
            drop(child_handle);
        }
    }

    /// Retain the result and its original task identity together until MCP ACK.
    /// The recent task display may evict its copy before a consumer reconnects.
    /// broadcast send 在无 subscriber 时返回错误，忽略即可。
    pub fn complete(
        &self,
        ticket: &WorkflowRunTicket,
        result: WorkflowTaskResult,
        mut task_record: TaskRecord,
    ) -> bool {
        let run_id = ticket.run_id();
        let mut runs = self.runs.lock();
        if result.run_id != run_id
            || task_record.task_id != run_id
            || self.registrations.lock().get(run_id) != Some(ticket)
        {
            return false;
        }
        let mut completions = self.completions.lock();
        if completions.contains_key(run_id) {
            return false;
        }
        if let Some(run) = runs.get_mut(run_id) {
            run.status = result.status;
        }
        task_record.status = match result.status {
            WorkflowRunStatus::Running => "running",
            WorkflowRunStatus::Completed => "completed",
            WorkflowRunStatus::Failed => "failed",
            WorkflowRunStatus::Killed => "cancelled",
        }
        .into();
        task_record.duration_ms = result.duration_ms;
        completions.insert(
            run_id.to_owned(),
            PendingCompletion {
                result: result.clone(),
                task_record,
            },
        );
        drop(completions);
        drop(runs);
        // Optional in-owner observers are wakeups; the retained MCP outbox is authoritative.
        let _ = self.notification_tx.send(result);
        true
    }

    pub fn acknowledge_observation(&self, run_id: &str) {
        if let Some(ticket) = self.registrations.lock().get(run_id) {
            ticket.acknowledge_observation();
        }
    }

    /// Retained until the MCP consumer confirms terminal delivery; broadcast is only an internal wakeup.
    pub fn pending_completions(&self) -> Vec<WorkflowTaskResult> {
        self.completions
            .lock()
            .values()
            .map(|pending| pending.result.clone())
            .collect()
    }

    /// Snapshot both halves under one lock; an ACK cannot split a completion
    /// from the task record needed to accept it after display eviction.
    pub fn pending_completion_records(&self) -> Vec<(TaskRecord, WorkflowTaskResult)> {
        self.completions
            .lock()
            .values()
            .map(|pending| (pending.task_record.clone(), pending.result.clone()))
            .collect()
    }

    /// Business eligibility comes from this source, never the bounded display.
    /// Read active admission and the terminal outbox under the same lock order
    /// as complete/ACK so an ACK cannot expose a stale owner Running record.
    pub fn background_tasks(
        &self,
    ) -> (
        std::collections::HashSet<String>,
        Vec<(TaskRecord, WorkflowTaskResult)>,
    ) {
        let runs = self.runs.lock();
        let completions = self.completions.lock();
        let running = runs
            .values()
            .filter(|run| run.status == WorkflowRunStatus::Running)
            .map(|run| run.run_id.clone())
            .collect();
        let pending = completions
            .values()
            .map(|pending| (pending.task_record.clone(), pending.result.clone()))
            .collect();
        (running, pending)
    }

    pub fn acknowledge_completion(&self, run_id: &str) {
        let mut runs = self.runs.lock();
        let mut completions = self.completions.lock();
        if completions.remove(run_id).is_none() {
            return;
        }
        let mut registrations = self.registrations.lock();
        if let Some(ticket) = registrations.remove(run_id) {
            ticket.acknowledge_observation();
        }
        runs.remove(run_id);
        release_excess_capacity(&mut runs);
        release_excess_capacity(&mut registrations);
        release_excess_capacity(&mut completions);
    }

    /// Only the admission path may discard a reservation before execution is
    /// spawned. A running cancellation retains its receipt until completion ACK.
    pub(crate) fn discard_unstarted(&self, ticket: &WorkflowRunTicket) {
        let mut runs = self.runs.lock();
        let mut registrations = self.registrations.lock();
        if registrations.get(ticket.run_id()) == Some(ticket) {
            runs.remove(ticket.run_id());
            registrations.remove(ticket.run_id());
            release_excess_capacity(&mut runs);
            release_excess_capacity(&mut registrations);
        }
    }

    /// 终止指定 workflow，移除记录并发送 kill 信号。
    ///
    /// 返回「被终止的是一条**运行中**的 run」：已终态（仅留档）的记录同样移除，
    /// 但返回 `false`——调用方据此区分「确实终止了运行中的 run」与「仅清理留档」。
    pub fn kill(&self, run_id: &str) -> Result<bool, RegistryError> {
        let run = self
            .runs
            .lock()
            .remove(run_id)
            .ok_or_else(|| RegistryError::NotFound(run_id.into()))?;
        let was_running = run.status == WorkflowRunStatus::Running;
        if let Some(kill_tx) = run.kill_tx {
            let _ = kill_tx.send(());
        }
        // kill_tx 触发 runner 的 kill 分支（runner.rs:425），其中完成所有清理：
        // workflow/kill RPC → child.kill().await → state.json → done_tx.send()
        // → active_channels.remove() → progress_store.cleanup_completed()
        // 不在此处 abort child_handle，避免中断 runner 清理路径。
        Ok(was_running)
    }

    /// 列出所有 workflow run 的摘要信息。
    pub fn list_runs(&self) -> Vec<(String, WorkflowRunStatus, String)> {
        let runs = self.runs.lock();
        runs.values()
            .map(|r| (r.run_id.clone(), r.status, r.workflow_name.clone()))
            .collect()
    }
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
